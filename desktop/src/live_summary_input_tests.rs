use gpui::{Modifiers, TestAppContext, px};
use serde_json::json;

use super::{InputDriver, InputHarness, accepted_snapshot};

#[gpui::test]
fn overview_summary_cards_show_selected_resources_and_navigate_to_them(cx: &mut TestAppContext) {
    // Display the prototype's overview readings and open CPU through its installed summary card.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_summary_readings();

    assert!(harness.driver.reading_is_visible("summary-card:CPU:25.0 %"));
    assert!(harness.driver.reading_is_visible("summary-card:GPU:40.0 %"));
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Memory pressure:Warning")
    );
    harness.driver.click_summary_card("summary-card:CPU:25.0 %");

    harness.driver.assert_summary_destination("cpu");
    harness.teardown();
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
            snapshot.selection["trends"][1]["pressure"] = json!(1);
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
