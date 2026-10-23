use gpui::{
    Bounds, Context, Div, MouseDownEvent, MouseUpEvent, Pixels, div, prelude::*, px, relative,
};

use crate::chart_axes::TimeWindow;
use crate::charts::{self, BLUE};
use crate::live_dashboard::{Drag, LiveDashboard};
use crate::live_hover::Origin;
use crate::time_range::Range;

/// How far the pointer may move, across or down, between a press and its release for the two
/// to be a click rather than a drag.
const CLICK_TRAVEL_MAX: Pixels = px(3.0);

impl LiveDashboard {
    /// Starts selecting a stretch on `chart`, from the second under the pointer.
    pub(crate) fn drag_begin(
        &mut self,
        chart: &str,
        (bounds, event): (Bounds<Pixels>, &MouseDownEvent),
        cx: &mut Context<Self>,
    ) {
        let second = self.second_under(bounds, event.position.x);

        self.drag_begin_at(chart, (second, event.position), cx);
    }

    /// Starts selecting a stretch on `chart`, from `second`, which the pointer pressed at
    /// `pressed_at`.
    pub(crate) fn drag_begin_at(
        &mut self,
        chart: &str,
        (second, pressed_at): (Option<u64>, gpui::Point<Pixels>),
        cx: &mut Context<Self>,
    ) {
        let Some(second) = second else {
            return;
        };
        self.drag = Some(Drag {
            chart: chart.to_owned(),
            from: second,
            to: second,
            pressed_at,
        });
        cx.notify();
    }

    /// Stretches the selection on `chart` to the second under the pointer.
    pub(crate) fn drag_move(
        &mut self,
        chart: &str,
        bounds: Bounds<Pixels>,
        x: Pixels,
        cx: &mut Context<Self>,
    ) {
        let second = self.second_under(bounds, x);

        self.drag_move_at(chart, second, cx);
    }

    /// Stretches the selection on `chart` to `second`.
    pub(crate) fn drag_move_at(
        &mut self,
        chart: &str,
        second: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let Some(second) = second else {
            return;
        };
        let Some(drag) = &mut self.drag else {
            return;
        };
        if drag.chart != chart || drag.to == second {
            return;
        }
        drag.to = second;
        cx.notify();
    }

    /// Ends the selection where the pointer was let go, and shows the stretch it covers.
    pub(crate) fn drag_end(
        &mut self,
        chart: &str,
        (bounds, event): (Bounds<Pixels>, &MouseUpEvent),
        cx: &mut Context<Self>,
    ) {
        let second = self.second_under(bounds, event.position.x);

        self.drag_end_at(chart, (second, event.position), cx);
    }

    /// Ends the selection on `chart` at `second`, which the pointer let go at `released_at`, and
    /// shows the stretch it covers. A release that barely moved from the press is a click, which
    /// pins the reading under the pointer.
    pub(crate) fn drag_end_at(
        &mut self,
        chart: &str,
        (second, released_at): (Option<u64>, gpui::Point<Pixels>),
        cx: &mut Context<Self>,
    ) {
        // Every chart is told of a release outside it, so only the chart being dragged ends it.
        if self.drag.as_ref().is_none_or(|drag| drag.chart != chart) {
            return;
        }
        self.drag_move_at(chart, second, cx);
        let Some(drag) = self.drag.take() else {
            return;
        };
        cx.notify();
        if click(drag.pressed_at, released_at) {
            match self.drag_end_hovered(chart) {
                Some(target) => self.pin_target(target, cx),
                None => self.pin_second(drag.from, cx),
            }
        } else {
            if let Some(range) = Range::selection(drag.from, drag.to) {
                self.select_range(range, cx);
            }
        }
    }

    /// The reading the pointer's hover of `chart` shows, which is the exact observation under
    /// it, as on the CPU history where one second holds several.
    fn drag_end_hovered(&self, chart: &str) -> Option<crate::live_hover::Target> {
        let preview = self.preview.as_ref()?;
        if preview.origin != Origin::Pointer {
            return None;
        }
        if preview.source.as_ref() != chart {
            return None;
        }

        Some(preview.target.clone())
    }

    /// The second at the pointer's horizontal place across the plot, held to the plot's edges.
    fn second_under(&self, bounds: Bounds<Pixels>, x: Pixels) -> Option<u64> {
        let rect = charts::plot_bounds(bounds);
        let fraction = f32::from(x - rect.origin.x) / f32::from(rect.size.width);

        self.view_window()?.second_at(fraction.clamp(0.0, 1.0))
    }

    /// The band that marks the stretch being selected on `chart`, or else the pinned second.
    pub(crate) fn drag_band(&self, chart: &str) -> Option<Div> {
        self.drag_band_in(chart, self.view_window()?)
    }

    /// The band that marks the stretch being selected on `chart`, which draws `window`, or else
    /// the pinned second, which every chart marks as the press that pinned it did.
    pub(crate) fn drag_band_in(&self, chart: &str, window: TimeWindow) -> Option<Div> {
        let ends = if let Some(drag) = self.drag.as_ref().filter(|drag| drag.chart == chart) {
            (drag.from, drag.to)
        } else {
            let second = self.pinned_second()?;
            (second, second)
        };
        let (left, right) = band_edges(window, ends)?;
        let selector = format!("chart-band:{chart}");

        Some(
            div()
                .debug_selector(move || selector)
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(left))
                .w(relative(right - left))
                .bg(charts::alpha(BLUE, 0.2))
                .border_l(px(1.0))
                .border_r(px(1.0))
                .border_color(charts::alpha(BLUE, 0.8)),
        )
    }
}

/// Whether a press at `pressed_at` and its release at `released_at` are a click.
fn click(pressed_at: gpui::Point<Pixels>, released_at: gpui::Point<Pixels>) -> bool {
    let travel = released_at - pressed_at;
    if travel.x.abs() > CLICK_TRAVEL_MAX {
        return false;
    }

    travel.y.abs() <= CLICK_TRAVEL_MAX
}

/// The left and right edges, as fractions of the plot, of the band covering every second
/// between `ends`, which may come in either order.
fn band_edges(window: TimeWindow, ends: (u64, u64)) -> Option<(f32, f32)> {
    let first = ends.0.min(ends.1);
    let end = ends.0.max(ends.1).checked_add(1)?;
    let span = u16::try_from(window.span).ok()?;
    let left = u16::try_from(first.checked_sub(window.first)?).ok()?;
    let right = u16::try_from(end.checked_sub(window.first)?).ok()?;

    Some((
        f32::from(left) / f32::from(span),
        f32::from(right.min(span)) / f32::from(span),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_band_covers_every_second_between_the_two_ends_of_the_drag() {
        // Goal: the band marks whole seconds from the earlier end to the later, whichever way
        // the drag went. Method: build the band of a drag in both directions in a 100-second
        // window.
        let window = TimeWindow {
            first: 1000,
            span: 100,
        };

        assert_eq!(band_edges(window, (1010, 1029)), Some((0.1, 0.3)));
        assert_eq!(band_edges(window, (1029, 1010)), Some((0.1, 0.3)));
    }
}
