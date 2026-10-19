//! The prototype's QA controls: four sliders that override a reading on the schematic while
//! the charts keep the recording's data.

use std::rc::Rc;

use gpui::{
    Context, DragMoveEvent, IntoElement, MouseButton, MouseDownEvent, Pixels, Render, Window,
    canvas, div, prelude::*, px, rgb,
};

use crate::charts::{BLUE, LINE, MUTED, TEXT};
use crate::hardware_motion::HardwareState;
use crate::live_dashboard::LiveDashboard;

/// One slider: what it overrides, and the range and step the prototype gives it.
struct Spec {
    name: &'static str,
    unit: &'static str,
    min: f32,
    max: f32,
    step: f32,
    digits: usize,
}

const SPECS: [Spec; 4] = [
    Spec {
        name: "CPU temperature",
        unit: " °C",
        min: 20.0,
        max: 110.0,
        step: 0.1,
        digits: 1,
    },
    Spec {
        name: "GPU temperature",
        unit: " °C",
        min: 20.0,
        max: 110.0,
        step: 0.1,
        digits: 1,
    },
    Spec {
        name: "Fan 0 speed",
        unit: " rpm",
        min: 0.0,
        max: 8000.0,
        step: 1.0,
        digits: 0,
    },
    Spec {
        name: "Fan 1 speed",
        unit: " rpm",
        min: 0.0,
        max: 8000.0,
        step: 1.0,
        digits: 0,
    },
];

/// The width of the slider's thumb, which stays inside the track at both ends.
const THUMB: f32 = 16.0;
const TRACK: f32 = 8.0;

/// A drag of one slider's thumb, by its index among the controls.
#[derive(Clone, Copy)]
struct SliderDrag(usize);

/// The drag preview, which draws nothing since the thumb itself moves.
struct NoPreview;

impl Render for NoPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_0()
    }
}

impl LiveDashboard {
    /// The controls' panel. `values` are the readings the schematic shows now, in the controls'
    /// order.
    pub(crate) fn hardware_controls(
        &self,
        values: [Option<f32>; 4],
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let state = Rc::clone(&self.hardware_state);
        let simulated = state.simulation.get().iter().any(Option::is_some);
        let reset = div()
            .id("hardware-controls-reset")
            .debug_selector(|| "hardware-controls-reset".into())
            .flex()
            .items_center()
            .flex_shrink_0()
            .h(px(44.0))
            .px(px(12.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(LINE))
            .text_size(px(12.0))
            .line_height(px(18.0))
            .text_color(rgb(TEXT))
            .child("Reset to recording");
        let reset = if simulated {
            reset
                .cursor_pointer()
                .hover(|style| style.bg(rgb(0x2c_2f_35)))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.hardware_state.simulation.set([None; 4]);
                    cx.notify();
                }))
                .into_any_element()
        } else {
            reset.opacity(0.4).into_any_element()
        };

        let mut sliders = Vec::with_capacity(SPECS.len());
        for (index, spec) in SPECS.iter().enumerate() {
            sliders.push(slider(index, spec, values[index], &state, cx));
        }

        div()
            .debug_selector(|| "hardware-controls".into())
            .p(px(16.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(LINE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.0))
                    .mb(px(12.0))
                    .child(
                        div()
                            .max_w(px(491.3))
                            .text_size(px(12.0))
                            .line_height(px(18.0))
                            .text_color(rgb(MUTED))
                            .child(
                                "Drag to override a reading. Charts keep captured data. \
                                Slider ranges are for testing, not device limits.",
                            ),
                    )
                    .child(reset),
            )
            .child(div().flex().gap(px(24.0)).children(sliders))
    }
}

fn slider(
    index: usize,
    spec: &'static Spec,
    value: Option<f32>,
    state: &Rc<HardwareState>,
    cx: &mut Context<LiveDashboard>,
) -> gpui::Div {
    let text = value.map_or_else(
        || "Unavailable".to_owned(),
        |value| {
            format!(
                "{}{}",
                crate::live_projection::fixed_text(value, spec.digits),
                spec.unit
            )
        },
    );
    let fraction = value.map_or(0.0, |value| {
        ((value - spec.min) / (spec.max - spec.min)).clamp(0.0, 1.0)
    });

    div()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_size(px(12.0))
                        .line_height(px(18.0))
                        .text_color(rgb(MUTED))
                        .child(spec.name),
                )
                .child(div().text_size(px(13.0)).child(text)),
        )
        .child(slider_track(index, spec, fraction, state, cx))
}

/// Moves the override to where the pointer is on the track, snapped to the control's step.
fn slide(index: usize, spec: &Spec, x: Pixels, state: &HardwareState) {
    let track = state.tracks.get()[index];
    let travel = f32::from(track.size.width) - THUMB;
    if travel <= 0.0 {
        return;
    }
    let along = (f32::from(x - track.origin.x) - THUMB / 2.0) / travel;
    let raw = spec.min + along.clamp(0.0, 1.0) * (spec.max - spec.min);
    let snapped = ((raw / spec.step).round() * spec.step).clamp(spec.min, spec.max);
    let mut simulation = state.simulation.get();
    simulation[index] = Some(snapped);
    state.simulation.set(simulation);
}

fn slider_track(
    index: usize,
    spec: &'static Spec,
    fraction: f32,
    state: &Rc<HardwareState>,
    cx: &mut Context<LiveDashboard>,
) -> gpui::Stateful<gpui::Div> {
    let (measured, down, drag) = (Rc::clone(state), Rc::clone(state), Rc::clone(state));
    let middle = px((44.0 - TRACK) / 2.0);

    div()
        .id(("hardware-control", index))
        .debug_selector(move || format!("hardware-control:{}", spec.name))
        .relative()
        .mt(px(4.0))
        .h(px(44.0))
        .cursor_pointer()
        .child(
            canvas(
                move |track, _, _| {
                    let mut tracks = measured.tracks.get();
                    tracks[index] = track;
                    measured.tracks.set(tracks);
                },
                |_, (), _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top(middle)
                .h(px(TRACK))
                .rounded(px(TRACK / 2.0))
                .border_1()
                .border_color(rgb(0x76_76_76))
                .bg(rgb(0x3b_3b_3b)),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .top(middle)
                .h(px(TRACK))
                .w(gpui::relative(fraction))
                .rounded(px(TRACK / 2.0))
                .bg(rgb(BLUE)),
        )
        .child(
            div()
                .absolute()
                .top(px((44.0 - THUMB) / 2.0))
                .left(gpui::relative(fraction))
                .ml(px(-THUMB * fraction))
                .size(px(THUMB))
                .rounded_full()
                .bg(rgb(BLUE)),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_, event: &MouseDownEvent, _, cx| {
                slide(index, spec, event.position.x, &down);
                cx.notify();
            }),
        )
        .on_drag(SliderDrag(index), |_, _, _, cx| cx.new(|_| NoPreview))
        .on_drag_move(
            cx.listener(move |_, event: &DragMoveEvent<SliderDrag>, _, cx| {
                if event.drag(cx).0 == index {
                    slide(index, spec, event.event.position.x, &drag);
                    cx.notify();
                }
            }),
        )
}
