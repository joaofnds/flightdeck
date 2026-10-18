use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    Bounds, Context, IntoElement, MouseButton, Pixels, Window, canvas, div, fill, point,
    prelude::*, px, rgb, size,
};
use num_traits::ToPrimitive;

use super::{CPUHistory, CPUInspection, CPUStep, PAGE_CPUS};
use crate::charts::{GREEN, GRID, MUTED, ORANGE, PANEL, RED, TEXT};
use crate::dashboard::control;
use crate::live_dashboard::LiveDashboard;
use crate::live_hover::{Origin, Target};

const ROW_HEIGHT: f32 = 18.0;
const HOVER_SOURCE: &str = "cpu-history-plot";

impl LiveDashboard {
    pub fn cpu_history_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = div()
            .p(px(16.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(div().text_size(px(15.0)).child("Logical CPU history"))
            .child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child(
                "Busy scheduler ticks over 15 minutes. Blank intervals are unavailable. \
                 CPU indices do not identify physical cores or clusters.",
            ));
        let Some(history) = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.cpu_history.as_ref())
        else {
            return panel.child("Logical CPU history unavailable.");
        };

        let mut panel = panel;
        if history.wire.retention_limited {
            panel = panel.child("Some CPU history is unavailable in this 15-minute window.");
        }
        if let Some(message) = &history.wire.restoration_error {
            panel = panel.child(
                div()
                    .id("cpu-history-restoration-error")
                    .debug_selector(|| "cpu-history-restoration-error".into())
                    .accessibility_id("flightdeck.cpu-history.restoration-error")
                    .role(gpui::Role::Status)
                    .aria_label(message.clone())
                    .text_color(rgb(ORANGE))
                    .child(message.clone()),
            );
        }
        if history.cpus().is_empty() {
            return panel.child("Logical CPU history unavailable.");
        }

        let inspection = self.cpu_history_inspection(history);
        panel = panel
            .child(Self::cpu_history_pages(history, cx))
            .child(self.cpu_history_plot(history, cx))
            .child(cpu_history_times(history))
            .child(cpu_history_readout(history, inspection));
        panel.child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child(
            "0–59% green · 60–79% yellow · 80–89% orange · 90–100% red · \
             Blank: unavailable",
        ))
    }

    fn cpu_history_pages(history: &CPUHistory, cx: &mut Context<Self>) -> impl IntoElement {
        let previous = history.offset().checked_sub(PAGE_CPUS);
        let next = history.offset() + PAGE_CPUS;
        let next = (next < history.total()).then_some(next);
        let Ok(count) = u32::try_from(history.cpus().len()) else {
            unreachable!("Validated CPU history has at most 32 rows");
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .mt(px(12.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "Logical CPUs {}–{} of {} · 15-minute context",
                        history.offset(),
                        history.offset() + count - 1,
                        history.total()
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .child(
                        control("Previous")
                            .accessibility_id("flightdeck.cpu-history.previous")
                            .aria_label("Previous logical CPU history page")
                            .when(previous.is_none(), |button| {
                                button.opacity(0.35).cursor_default()
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(offset) = previous {
                                    this.request.cpu_offset = offset;
                                    this.submit();
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        control("Next")
                            .accessibility_id("flightdeck.cpu-history.next")
                            .aria_label("Next logical CPU history page")
                            .when(next.is_none(), |button| {
                                button.opacity(0.35).cursor_default()
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(offset) = next {
                                    this.request.cpu_offset = offset;
                                    this.submit();
                                    cx.notify();
                                }
                            })),
                    ),
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
        let plot = div()
            .id(HOVER_SOURCE)
            .debug_selector(|| HOVER_SOURCE.into())
            .track_focus(focus)
            .tab_stop(true)
            .focus(|style| style.bg(rgb(0x20_30_41)))
            .flex()
            .gap(px(6.0))
            .child(cpu_history_rows(
                history,
                self.cpu_history_inspection(history),
            ))
            .child(self.cpu_history_cells(history, cx))
            .on_key_down(cx.listener(Self::cpu_history_keyboard));

        self.cpu_history_accessibility(plot, history, cx)
    }

    fn cpu_history_cells(
        &self,
        history: &Arc<CPUHistory>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let measured = Rc::new(Cell::new(Bounds::default()));
        let measure = Rc::clone(&measured);
        let paint_history = Arc::clone(history);
        let inspection = self.cpu_history_inspection(history);
        let Ok(rows) = u8::try_from(history.cpus().len()) else {
            unreachable!("Validated CPU history has at most 32 rows");
        };
        let plot = canvas(
            move |bounds, _, _| measure.set(bounds),
            move |bounds, (), window, _| {
                cpu_history_paint(&paint_history, inspection, bounds, window);
            },
        )
        .w_full()
        .h(px(f32::from(rows) * ROW_HEIGHT));
        let move_history = Arc::clone(history);
        let move_bounds = Rc::clone(&measured);
        let click_history = Arc::clone(history);
        let click_bounds = Rc::clone(&measured);
        let hover_history = Arc::clone(history);
        div()
            .id("cpu-history-cells")
            .debug_selector(|| "cpu-history-cells".into())
            .flex_1()
            .min_w_0()
            .relative()
            .child(plot)
            .children(self.reading_membership_observer(HOVER_SOURCE.into(), cx))
            .on_mouse_move(
                cx.listener(move |this, event: &gpui::MouseMoveEvent, _, cx| {
                    this.cpu_history_hover(&move_history, move_bounds.get(), event.position, cx);
                }),
            )
            .on_hover(cx.listener(move |this, hovered, window, cx| {
                if *hovered {
                    this.cpu_history_hover(
                        &hover_history,
                        measured.get(),
                        window.mouse_position(),
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
        let row = (f32::from(position.y - bounds.origin.y) / ROW_HEIGHT)
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
        self.set_reading_target(HOVER_SOURCE.into(), target, Origin::Pointer, cx);
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

fn cpu_history_rows(history: &CPUHistory, inspection: Option<CPUInspection>) -> impl IntoElement {
    let mut labels = div()
        .w(px(46.0))
        .flex_shrink_0()
        .text_size(px(10.0))
        .text_color(rgb(MUTED));
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
                .h(px(ROW_HEIGHT))
                .child(format!("CPU {}", cpu.cpu())),
        );
    }

    labels
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

fn cpu_history_readout(history: &CPUHistory, inspection: Option<CPUInspection>) -> gpui::Div {
    let reading = cpu_history_reading(history, inspection);
    let mut row = div()
        .flex()
        .flex_wrap()
        .text_size(px(11.0))
        .text_color(rgb(MUTED));
    for (index, part) in reading.split(" · ").take(4).enumerate() {
        if index > 0 {
            row = row.child(" · ");
        }
        let part = part.to_owned();
        row = row.child(div().debug_selector(|| part.clone()).child(part));
    }
    row
}

fn cpu_history_times(history: &CPUHistory) -> impl IntoElement {
    let middle = history.wire.first_ns + (history.wire.end_ns - history.wire.first_ns) / 2;
    div()
        .flex()
        .pl(px(52.0))
        .justify_between()
        .text_size(px(10.0))
        .text_color(rgb(MUTED))
        .children(
            [history.wire.first_ns, middle, history.wire.end_ns]
                .map(|wall_ns| crate::live_presentation::clock(wall_ns / super::NS, false)),
        )
}

fn cpu_history_paint(
    history: &CPUHistory,
    inspection: Option<CPUInspection>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    for (row, cpu) in history.cpus().iter().enumerate() {
        let Ok(row) = u8::try_from(row) else {
            unreachable!("Validated CPU history has at most 32 rows");
        };
        let top = bounds.origin.y + px(f32::from(row) * ROW_HEIGHT);
        window.paint_quad(fill(
            Bounds::new(
                point(bounds.origin.x, top),
                size(bounds.size.width, px(ROW_HEIGHT - 2.0)),
            ),
            rgb(0x10_14_1b),
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
                    size(bounds.size.width * (end - first), px(ROW_HEIGHT - 2.0)),
                );
                window.paint_quad(fill(rectangle, cpu_history_color(value)));
            }
        }
    }
    cpu_history_marker(
        history,
        history.wire.observation.wall_ns,
        bounds,
        TEXT,
        window,
    );
    if let Some(CPUInspection::Sample {
        observation_index, ..
    }) = inspection
    {
        if let Some(sample) = history.wire.observations.get(observation_index) {
            cpu_history_marker(
                history,
                sample.observation.wall_ns,
                bounds,
                0x6d_a9_ff,
                window,
            );
        }
    }
}

fn cpu_history_color(value: f64) -> gpui::Rgba {
    let color = if value >= 90.0 {
        RED
    } else {
        if value >= 80.0 {
            ORANGE
        } else {
            if value >= 60.0 { 0xf2_cc_0d } else { GREEN }
        }
    };
    let Some(opacity) = (0.24 + (value / 5.0).floor() * 0.038).to_f32() else {
        unreachable!("Validated CPU percentages fit f32");
    };

    crate::charts::alpha(color, opacity)
}

fn cpu_history_marker(
    history: &CPUHistory,
    wall_ns: u64,
    bounds: Bounds<Pixels>,
    color: u32,
    window: &mut Window,
) {
    let Some(fraction) = history.fraction(wall_ns).to_f32() else {
        unreachable!("Validated CPU history fractions fit f32");
    };
    window.paint_quad(fill(
        Bounds::new(
            point(
                bounds.origin.x + bounds.size.width * fraction,
                bounds.origin.y,
            ),
            size(px(1.0), bounds.size.height),
        ),
        crate::charts::alpha(color, 0.8),
    ));
}
