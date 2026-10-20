use gpui::{
    Bounds, Context, Div, MouseDownEvent, MouseUpEvent, Pixels, div, prelude::*, px, relative,
};

use crate::chart_axes::TimeWindow;
use crate::charts::{self, BLUE};
use crate::live_dashboard::{Drag, LiveDashboard};
use crate::time_range::Range;

impl LiveDashboard {
    /// Starts selecting a stretch on `chart`, from the second under the pointer.
    pub(crate) fn drag_begin(
        &mut self,
        chart: &str,
        (bounds, event): (Bounds<Pixels>, &MouseDownEvent),
        cx: &mut Context<Self>,
    ) {
        let Some(second) = self.second_under(bounds, event.position.x) else {
            return;
        };
        self.drag = Some(Drag {
            chart: chart.to_owned(),
            from: second,
            to: second,
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
        let Some(second) = self.second_under(bounds, x) else {
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
        // Every chart is told of a release outside it, so only the chart being dragged ends it.
        if self.drag.as_ref().is_none_or(|drag| drag.chart != chart) {
            return;
        }
        self.drag_move(chart, bounds, event.position.x, cx);
        let Some(drag) = self.drag.take() else {
            return;
        };
        cx.notify();
        if let Some(range) = Range::selection(drag.from, drag.to) {
            self.select_range(range, cx);
        }
    }

    /// The second at the pointer's horizontal place across the plot, held to the plot's edges.
    fn second_under(&self, bounds: Bounds<Pixels>, x: Pixels) -> Option<u64> {
        let rect = charts::plot_bounds(bounds);
        let fraction = f32::from(x - rect.origin.x) / f32::from(rect.size.width);

        self.view_window()?.second_at(fraction.clamp(0.0, 1.0))
    }

    /// The band that marks the stretch being selected on `chart`.
    pub(crate) fn drag_band(&self, chart: &str) -> Option<Div> {
        let drag = self.drag.as_ref().filter(|drag| drag.chart == chart)?;
        let window = self.view_window()?;
        let (left, right) = band_edges(window, drag)?;

        Some(
            div()
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

/// The band's left and right edges as fractions of the plot, covering every second it spans.
fn band_edges(window: TimeWindow, drag: &Drag) -> Option<(f32, f32)> {
    let first = drag.from.min(drag.to);
    let end = drag.from.max(drag.to).checked_add(1)?;
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
        let forward = Drag {
            chart: "p".into(),
            from: 1010,
            to: 1029,
        };
        let backward = Drag {
            chart: "p".into(),
            from: 1029,
            to: 1010,
        };

        assert_eq!(band_edges(window, &forward), Some((0.1, 0.3)));
        assert_eq!(band_edges(window, &backward), Some((0.1, 0.3)));
    }
}
