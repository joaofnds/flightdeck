use gpui::{Modifiers, TestAppContext};
use serde_json::json;

use super::{InputDriver, InputHarness, accepted_snapshot};

#[gpui::test]
fn recording_details_open_on_demand_without_crowding_the_readings(cx: &mut TestAppContext) {
    // Goal: keep source diagnostics available behind a disclosure instead of above every chart.
    // Method: open and close the installed control while observing its captured recording path.
    let mut harness = InputHarness::setup(cx);
    harness
        .driver
        .view
        .update(&mut harness.driver.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.catalog["path"] = json!("/captured/mac.recording");
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
    harness.driver.context.run_until_parked();
    assert!(!harness.driver.recording_details_visible());

    harness.driver.click_recording_details();

    assert!(harness.driver.recording_details_visible());
    assert!(
        harness
            .driver
            .context
            .debug_bounds("recording-path:/captured/mac.recording")
            .is_some()
    );
    harness.driver.click_recording_details();
    assert!(!harness.driver.recording_details_visible());
    harness.teardown();
}

impl InputDriver {
    fn recording_details_visible(&mut self) -> bool {
        self.context.debug_bounds("recording-details").is_some()
    }

    fn click_recording_details(&mut self) {
        let toggle = self
            .context
            .debug_bounds("recording-details-toggle")
            .unwrap();
        self.context
            .simulate_click(toggle.center(), Modifiers::none());
        self.context.run_until_parked();
    }
}
