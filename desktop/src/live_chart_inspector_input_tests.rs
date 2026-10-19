use gpui::{Modifiers, TestAppContext};
use serde_json::json;

use super::{InputDriver, InputHarness, PlotExit};

#[gpui::test]
fn chart_data_opens_the_visible_series_statistics_and_closes(cx: &mut TestAppContext) {
    // Open the production Data control and read the composed sample statistics, then dismiss it.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_chart_sample_gaps();

    harness.driver.chart_control("chart-data:live-energy");

    assert!(harness.driver.reading_is_visible("Sample mean"));
    assert!(harness.driver.reading_is_visible("Sample maximum"));
    assert!(harness.driver.reading_is_visible("Available samples"));
    assert!(harness.driver.reading_is_visible("49.0 W"));
    assert!(harness.driver.reading_is_visible("90.0 W"));
    assert!(harness.driver.reading_is_visible("2 / 3"));
    harness.driver.chart_control("chart-data-close");
    assert!(!harness.driver.reading_is_visible("Sample mean"));
    harness.teardown();
}

#[gpui::test]
fn separate_power_plots_share_inspection_and_remove_the_tooltip_on_exit(cx: &mut TestAppContext) {
    // GPU inspection drives the separate system plot at the same timestamp, then clears on exit.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_chart_sample_gaps();

    harness
        .driver
        .hover_previous_second_on("live-gpu-power-plot");

    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip:live-gpu-power")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip-row:live-gpu-power:0:GPU power:100.0 W")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-energy:90.0 W")
    );
    assert!(harness.driver.cursor_is_visible("live-energy-plot"));
    harness.driver.hover_selected_second_on("live-energy-plot");
    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip-row:live-energy:0:System power:8.0 W")
    );
    harness
        .driver
        .leave_plot("live-energy-plot", PlotExit::Sidebar);
    assert!(
        !harness
            .driver
            .reading_is_visible("chart-tooltip:live-energy")
    );
    harness.teardown();
}

impl InputDriver {
    fn seed_chart_sample_gaps(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = super::accepted_snapshot(dashboard.generation);
            let mut missing = snapshot.selection["trends"][0].clone();
            missing["wall_ns"] = json!(998_000_000_000_u64);
            missing["publication"] = json!(1);
            missing["power_watts"] = serde_json::Value::Null;
            let mut previous = snapshot.selection["trends"][0].clone();
            previous["publication"] = json!(2);
            previous["power_watts"] = json!(90);
            previous["gpu_watts"] = json!(100);
            let mut selected = snapshot.selection["trends"][1].clone();
            selected["publication"] = json!(3);
            snapshot.selection["selected"]["publication"] = json!(3);
            snapshot.selection["trends"] = json!([missing, previous, selected]);
            dashboard.request.identity = Some(snapshot.selection["selected"].clone());
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn chart_control(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).unwrap();
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }
}
