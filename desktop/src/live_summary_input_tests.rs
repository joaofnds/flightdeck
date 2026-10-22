use gpui::{Bounds, Modifiers, Pixels, TestAppContext, px};
use serde_json::json;

use super::{InputDriver, InputHarness, accepted_snapshot};

#[gpui::test]
fn overview_summary_cards_show_selected_resources_and_open_nothing(cx: &mut TestAppContext) {
    // Goal: the summary cards show the selected readings and a click on one changes nothing,
    // since only the sidebar navigates. Method: seed the readings, click the CPU card and read
    // the route.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_summary_readings();

    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:CPU:71.5 °C")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:GPU:63.2 °C")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Memory pressure:Warning")
    );
    harness
        .driver
        .click_summary_card("summary-card:CPU:71.5 °C");

    harness.driver.assert_summary_destination("overview");
    harness.teardown();
}

#[gpui::test]
fn overview_summary_uses_four_equal_bounded_columns_at_medium_width(cx: &mut TestAppContext) {
    // Measure all installed cards, including the partial second row, at the reference width.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_summary_readings();
    harness.driver.resize_summary_grid(840.0);

    harness.driver.assert_summary_grid_columns(4);

    harness.teardown();
}

#[gpui::test]
fn overview_summary_reflows_through_two_four_and_all_card_columns(cx: &mut TestAppContext) {
    // Cross both exact breakpoints with the same accepted readings and equal grid tracks.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_summary_readings();
    let selected = harness.driver.accepted_selection().unwrap()["selected"].clone();

    harness.driver.resize_summary_grid(639.0);
    harness.driver.assert_summary_grid_columns(2);
    harness.driver.resize_summary_grid(640.0);
    harness.driver.assert_summary_grid_columns(4);
    harness.driver.resize_summary_grid(1099.0);
    harness.driver.assert_summary_grid_columns(4);
    harness.driver.resize_summary_grid(1100.0);
    harness.driver.assert_summary_grid_columns(6);
    harness.driver.resize_summary_grid(639.0);

    harness.driver.assert_summary_grid_columns(2);
    assert_eq!(
        harness.driver.accepted_selection().unwrap()["selected"],
        selected
    );
    harness.teardown();
}

#[gpui::test]
fn summary_cards_keep_reference_padding_reading_height_and_note_spacing(cx: &mut TestAppContext) {
    // Read the actual content bounds inside the installed CPU summary card.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_summary_readings();
    harness.driver.resize_summary_grid(1800.0);
    let card = harness
        .driver
        .context
        .debug_bounds("summary-card:CPU:71.5 °C")
        .unwrap();
    let label = harness
        .driver
        .context
        .debug_bounds("summary-label:CPU")
        .unwrap();
    let value = harness
        .driver
        .context
        .debug_bounds("summary-value:CPU")
        .unwrap();
    let note = harness
        .driver
        .context
        .debug_bounds("summary-note:CPU")
        .unwrap();

    assert_eq!(label.left() - card.left(), px(13.0));
    assert_eq!(label.top() - card.top(), px(13.0));
    assert_eq!(card.right() - label.right(), px(13.0));
    assert_eq!(value.top() - label.bottom(), px(3.0));
    assert_eq!(value.size.height, px(30.0));
    assert_eq!(note.top() - value.bottom(), px(3.0));
    assert_eq!(note.size.height, px(16.0));
    assert_eq!(card.bottom() - note.bottom(), px(13.0));
    harness.teardown();
}

#[gpui::test]
fn a_wide_cpu_summary_fills_four_equal_tracks(cx: &mut TestAppContext) {
    // The four CPU readings fill the same content width as Overview's six readings.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_summary_readings();
    let cards = [
        "summary-card:CPU:25%",
        "summary-card:Performance cores:Unavailable",
        "summary-card:Super cores:Unavailable",
        "summary-card:Load average:Unavailable",
    ]
    .map(|selector| {
        harness
            .driver
            .context
            .debug_bounds(selector)
            .expect(selector)
    });
    let viewport = harness
        .driver
        .context
        .update(|window, _| window.viewport_size());

    assert_eq!(cards[0].left(), px(188.0));
    assert_eq!(cards[3].right(), viewport.width - px(24.0));
    for card in cards {
        assert_eq!(card.top(), cards[0].top());
        assert!((card.size.width - cards[0].size.width).abs() <= px(1.0));
    }
    for pair in cards.windows(2) {
        assert_eq!(pair[1].left() - pair[0].right(), px(8.0));
    }
    harness.teardown();
}

impl InputDriver {
    fn resize_summary_grid(&mut self, width: f32) {
        self.context
            .simulate_resize(gpui::size(px(width), px(3500.0)));
        self.context.run_until_parked();
    }

    fn overview_summary_bounds(&mut self) -> [Bounds<Pixels>; 6] {
        [
            "summary-card:CPU:71.5 °C",
            "summary-card:GPU:63.2 °C",
            "summary-card:Memory pressure:Warning",
            "summary-card:System power:8 W",
            "summary-card:Disk read:Unavailable",
            "summary-card:Receive:Unavailable",
        ]
        .map(|selector| self.context.debug_bounds(selector).expect(selector))
    }

    fn assert_summary_grid_columns(&mut self, columns: usize) {
        let cards = self.overview_summary_bounds();
        assert_eq!(
            cards
                .iter()
                .take_while(|card| card.top() == cards[0].top())
                .count(),
            columns
        );
        let viewport = self.context.update(|window, _| window.viewport_size());
        for card in cards {
            assert!((card.size.width - cards[0].size.width).abs() <= px(1.0));
            assert!(card.left() >= px(188.0));
            assert!(card.right() <= viewport.width - px(24.0));
        }
        assert_eq!(cards[1].left() - cards[0].right(), px(8.0));
        if columns < cards.len() {
            assert_eq!(cards[columns].left(), cards[0].left());
            assert_eq!(cards[columns].top() - cards[0].bottom(), px(8.0));
        }
    }
}

impl InputDriver {
    fn seed_summary_readings(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.selection["trends"][0]["cpu_percent"] = json!(10);
            snapshot.selection["trends"][0]["gpu_active_percent"] = json!(20);
            snapshot.selection["trends"][0]["pressure"] = json!(0);
            snapshot.selection["trends"][1]["cpu_percent"] = json!(25);
            snapshot.selection["trends"][1]["gpu_active_percent"] = json!(40);
            snapshot.selection["trends"][1]["cpu_celsius"] = json!(71.5);
            snapshot.selection["trends"][1]["gpu_celsius"] = json!(63.25);
            snapshot.selection["trends"][1]["pressure"] = json!(1);
            snapshot.selection["trends"][1]["performance_levels"] = json!([
                {"name": "Super", "logical_cpus": 6},
                {"name": "Performance", "logical_cpus": 12},
            ]);
            snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(3500.0)));
        self.context.run_until_parked();
    }

    fn click_summary_card(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).expect(selector);
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }

    fn assert_summary_destination(&self, route: &str) {
        self.view.read_with(&self.context, |dashboard, _| {
            assert_eq!(dashboard.request.route, route);
        });
    }
}
