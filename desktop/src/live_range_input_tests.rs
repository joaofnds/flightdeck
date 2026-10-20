use gpui::{Modifiers, MouseButton, TestAppContext};

use super::{InputDriver, InputHarness};
use crate::chart_axes::TimeWindow;

const PLOT: &str = "live-energy-plot";

impl InputDriver {
    fn drag_plot(&mut self, from: f32, to: f32) {
        let start = self.second_position(PLOT, from);
        let end = self.second_position(PLOT, to);
        self.context
            .simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        self.context
            .simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
        self.context
            .simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        self.context.run_until_parked();
    }

    fn drag_is_over(&self) -> bool {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.drag.is_none())
    }

    fn follow_live(&mut self) {
        self.view.update(&mut self.context, |dashboard, _| {
            dashboard.request.second = None;
            dashboard.request.identity = None;
        });
    }

    fn view_window(&self) -> Option<TimeWindow> {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.view_window())
    }

    fn energy_window(&self) -> Option<TimeWindow> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-energy")?
                .window
        })
    }

    fn click_control(&mut self, selector: &'static str) {
        let bounds = super::menu_control_bounds(&mut self.context, selector);
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }
}

#[gpui::test]
fn dragging_across_a_chart_zooms_the_charts_to_that_stretch_and_holds_it(cx: &mut TestAppContext) {
    // Goal: a drag selects a stretch of time, the charts show only that stretch, and the view
    // stops following the newest second so the stretch stays where it was drawn.
    // Method: follow the live second, drag from a quarter to three quarters of the plot, then
    // read the view, the chart's own window and the second the reader asks for.
    let mut harness = InputHarness::setup(cx);
    harness.driver.follow_live();

    harness.driver.drag_plot(0.25, 0.75);

    let stretch = TimeWindow {
        first: 326,
        span: 451,
    };
    assert_eq!(harness.driver.view_window(), Some(stretch));
    assert_eq!(harness.driver.energy_window(), Some(stretch));
    assert_eq!(harness.driver.requested_second(), Some(1000));
    assert!(harness.driver.drag_is_over());
    harness.teardown();
}

#[gpui::test]
fn dragging_right_to_left_selects_the_same_stretch(cx: &mut TestAppContext) {
    // Goal: the direction of a drag does not change what it selects.
    // Method: drag from three quarters to a quarter of the plot and read the view.
    let mut harness = InputHarness::setup(cx);

    harness.driver.drag_plot(0.75, 0.25);

    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: 326,
            span: 451
        })
    );
    harness.teardown();
}

#[gpui::test]
fn a_click_or_a_tiny_drag_leaves_the_range_alone(cx: &mut TestAppContext) {
    // Goal: only a deliberate drag changes the range, so a stray click never zooms.
    // Method: press and release on one point, then drag across a few seconds, and read the view.
    let mut harness = InputHarness::setup(cx);
    let full = harness.driver.view_window();

    harness.driver.drag_plot(0.5, 0.5);
    harness.driver.drag_plot(0.5, 0.503);

    assert_eq!(harness.driver.view_window(), full);
    harness.teardown();
}

#[gpui::test]
fn the_reset_button_returns_to_the_latest_fifteen_minutes(cx: &mut TestAppContext) {
    // Goal: one press undoes a zoom and goes back to following the newest second.
    // Method: zoom with a drag, press the reset button, and read the view and the request.
    let mut harness = InputHarness::setup(cx);
    harness.driver.follow_live();
    harness.driver.drag_plot(0.25, 0.75);

    harness.driver.click_control("time-reset");

    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: 101,
            span: 900
        })
    );
    assert_eq!(harness.driver.requested_second(), None);
    harness.teardown();
}

#[gpui::test]
fn choosing_a_shorter_range_shows_the_last_minutes_of_history(cx: &mut TestAppContext) {
    // Goal: the range menu offers the last few minutes, ending at the selected second.
    // Method: open the menu, choose the last five minutes, and read the view.
    let mut harness = InputHarness::setup(cx);

    harness.driver.click_control("time-range-5m");

    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: 701,
            span: 300
        })
    );
    harness.teardown();
}
