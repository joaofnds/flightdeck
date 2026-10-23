use gpui::{Modifiers, MouseButton, TestAppContext};

use super::{InputDriver, InputHarness};
use crate::chart_axes::TimeWindow;
use crate::time_range::Range;

const PLOT: &str = "live-energy-plot";

impl InputDriver {
    fn drag_plot(&mut self, from: f32, to: f32) {
        self.drag_across(PLOT, from, to);
    }

    /// Drags across the plot `selector` from the fraction `from` of its width to `to`.
    pub(super) fn drag_across(&mut self, selector: &'static str, from: f32, to: f32) {
        let start = self.second_position(selector, from);
        let end = self.second_position(selector, to);
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

    pub(super) fn show_range(&mut self, range: Range) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.set_range(range, cx);
        });
        self.context.run_until_parked();
    }

    pub(super) fn view_window(&self) -> Option<TimeWindow> {
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

    fn energy_sliding(&self) -> bool {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-energy")
                .is_some_and(|panel| panel.sliding)
        })
    }

    fn energy_chart_width(&self) -> Option<f32> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-energy")?
                .chart_width
                .get()
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
    // stops following the newest second so the stretch stays where it was drawn, with every
    // reading in it drawn.
    // Method: follow the live second, drag from a quarter to three quarters of the plot, then
    // read the view, the chart's own window, whether it slides, and the second the reader asks
    // for.
    let mut harness = InputHarness::setup(cx);
    harness.driver.follow_live();

    harness.driver.drag_plot(0.25, 0.75);

    let stretch = TimeWindow {
        first: 326,
        span: 451,
    };
    assert_eq!(harness.driver.view_window(), Some(stretch));
    assert_eq!(harness.driver.energy_window(), Some(stretch));
    assert!(!harness.driver.energy_sliding());
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
    // Method: zoom with a drag, press the reset button, and read the view, the request and
    // whether the chart slides.
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
    assert!(harness.driver.energy_sliding());
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

#[gpui::test]
fn a_short_range_keeps_a_selected_second_with_later_seconds_in_view(cx: &mut TestAppContext) {
    // Goal: a range shorter than the seconds the page holds after its selected second still
    // shows that second, at its middle.
    // Method: select 999 in a context that runs to 1100, show the last minute, and read the
    // view.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_selection_before_held_seconds(1100);

    harness.driver.show_range(Range::Last(60));

    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: 970,
            span: 60
        })
    );
    harness.teardown();
}

#[gpui::test]
fn choosing_thirty_minutes_or_an_hour_shows_all_the_history_there_is(cx: &mut TestAppContext) {
    // Goal: the longer ranges of the menu can be chosen, and a recording shorter than the range
    // shows what it has. Method: choose 30 minutes and then 60 minutes on a 1000-second
    // recording, and read the view after each.
    let mut harness = InputHarness::setup(cx);

    for selector in ["time-range-30m", "time-range-60m"] {
        harness.driver.click_control(selector);

        assert_eq!(
            harness.driver.view_window(),
            Some(TimeWindow {
                first: 0,
                span: 1001
            })
        );
    }
    harness.teardown();
}

#[gpui::test]
fn the_custom_stepper_goes_beyond_fifteen_minutes(cx: &mut TestAppContext) {
    // Goal: the stepper adds a minute to the default fifteen. Method: press plus once.
    let mut harness = InputHarness::setup(cx);

    harness.driver.click_control("time-custom-more");

    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: 41,
            span: 960
        })
    );
    harness.teardown();
}

#[gpui::test]
fn dragging_across_a_range_longer_than_fifteen_minutes_selects_inside_it(cx: &mut TestAppContext) {
    // Goal: drag-to-zoom works on the longer windows. Method: widen the range to 16 minutes,
    // drag from a quarter to three quarters of the plot, and read the stretch it holds.
    let mut harness = InputHarness::setup(cx);
    harness.driver.follow_live();
    harness.driver.click_control("time-custom-more");

    harness.driver.drag_plot(0.25, 0.75);

    assert_eq!(
        harness.driver.view_window(),
        Some(TimeWindow {
            first: 281,
            span: 481
        })
    );
    harness.teardown();
}

#[gpui::test]
fn a_chart_learns_how_wide_its_card_is_and_follows_a_resize(cx: &mut TestAppContext) {
    // Goal: a chart sizes its buckets for the card it is drawn in, and learns the new size when
    // the window changes.
    // Method: lay the page out at two window widths, and compare the width the energy panel
    // holds with the width inside its card's one-pixel border each time. Count the times the
    // page asks to be drawn again, since a page that learns a width without that keeps the old
    // buckets on screen until something else redraws it.
    let mut harness = InputHarness::setup(cx);
    let redraws = std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let counter = std::rc::Rc::clone(&redraws);
    let view = harness.driver.view.clone();
    let _observer = harness
        .driver
        .context
        .update(|_, cx| cx.observe(&view, move |_, _| counter.set(counter.get() + 1)));
    let mut widths = Vec::new();

    for width in [1320.0, 700.0] {
        let redraws_before = redraws.get();
        harness
            .driver
            .context
            .simulate_resize(gpui::size(gpui::px(width), gpui::px(4000.0)));
        harness.driver.context.run_until_parked();
        let card = harness.driver.chart_bounds("chart-card:live-energy");
        widths.push(f32::from(card.size.width) - 2.0);

        assert_eq!(harness.driver.energy_chart_width(), widths.last().copied());
        assert!(redraws.get() > redraws_before);
    }
    assert!(widths[1] < widths[0]);
    harness.teardown();
}

#[gpui::test]
fn a_chart_kept_apart_from_the_page_draws_for_its_own_card(cx: &mut TestAppContext) {
    // Goal: a chart the page keeps apart from its own panels, such as an app's history, still
    // draws buckets sized for the card it is laid out in.
    // Method: record a card width for a chart the page does not hold, size that chart for its
    // card as the page does when it draws it, and read the width it holds.
    let mut harness = InputHarness::setup(cx);
    let panel: crate::data::Panel = serde_json::from_value(serde_json::json!({
        "id":"app-history", "title":"History", "unit":"%", "kind":"line", "lines":[]}))
    .unwrap();
    harness
        .driver
        .view
        .update(&mut harness.driver.context, |dashboard, _| {
            let mut widths = dashboard.chart_widths.borrow_mut();
            widths.insert("app-history".to_owned(), gpui::px(900.0));
        });

    harness
        .driver
        .view
        .read_with(&harness.driver.context, |dashboard, _| {
            dashboard.size_for_card(&panel);
        });

    let drawn = panel.chart_width.get();

    assert_eq!(drawn, Some(900.0));
    harness.teardown();
}
