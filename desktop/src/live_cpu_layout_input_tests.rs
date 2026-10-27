use gpui::{Bounds, Pixels, TestAppContext, px};
use serde_json::json;
use std::sync::Arc;

use super::{InputDriver, InputHarness, accepted_snapshot};

#[gpui::test]
fn cpu_top_places_history_selected_bars_and_heatmap_beside_each_other(cx: &mut TestAppContext) {
    // Measure the installed CPU page rather than a separate layout fixture.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_cpu_layout_readings();
    harness.driver.resize_cpu_layout(1400.0);

    harness.driver.assert_cpu_layout_wide();

    harness.teardown();
}

#[gpui::test]
fn cpu_top_stacks_in_reference_order_below_the_wide_breakpoint(cx: &mut TestAppContext) {
    // Measure each production panel after rendering the page below the prototype breakpoint.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_cpu_layout_readings();
    harness.driver.resize_cpu_layout(1099.0);

    harness.driver.assert_cpu_layout_stacked();

    harness.teardown();
}

#[gpui::test]
fn cpu_top_reflows_across_the_breakpoint_without_losing_its_panels(cx: &mut TestAppContext) {
    // Cross both directions at the exact window breakpoint with the accepted snapshot retained.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_cpu_layout_readings();
    harness.driver.resize_cpu_layout(1099.0);
    harness.driver.assert_cpu_layout_stacked();

    harness.driver.resize_cpu_layout(1100.0);
    harness.driver.assert_cpu_layout_wide();
    harness.driver.resize_cpu_layout(1099.0);

    harness.driver.assert_cpu_layout_stacked();
    harness.teardown();
}

impl InputDriver {
    fn resize_cpu_layout(&mut self, width: f32) {
        self.context
            .simulate_resize(gpui::size(px(width), px(2400.0)));
        self.context.run_until_parked();
    }

    fn cpu_layout_bounds(&mut self) -> [Bounds<Pixels>; 3] {
        [
            self.context.debug_bounds("live-cpu-plot").unwrap(),
            self.context.debug_bounds("cpu-selected-panel").unwrap(),
            self.context.debug_bounds("cpu-heatmap-panel").unwrap(),
        ]
    }

    fn assert_cpu_layout_wide(&mut self) {
        let [history, selected, heatmap] = self.cpu_layout_bounds();
        assert!(history.right() < selected.left());
        assert!(selected.right() < heatmap.left());
        assert_eq!(selected.top(), heatmap.top());
        assert!(selected.top() < history.top());
        assert!(history.bottom() <= selected.bottom());
        assert!(heatmap.size.width > selected.size.width);
        let chart = self
            .context
            .debug_bounds("cpu-activity-history-panel")
            .unwrap();
        assert!((selected.left() - chart.right() - px(8.0)).abs() < px(0.5));
        assert!((heatmap.left() - selected.right() - px(8.0)).abs() < px(0.5));
        assert!((chart.size.width * 0.85 - selected.size.width * 1.35).abs() < px(2.0));
        assert!((heatmap.size.width * 0.85 - selected.size.width * 1.6).abs() < px(2.0));
        let viewport = self.context.update(|window, _| window.viewport_size());
        assert!(heatmap.right() <= viewport.width);
    }

    fn assert_cpu_layout_stacked(&mut self) {
        let [history, selected, heatmap] = self.cpu_layout_bounds();
        assert!(history.bottom() < selected.top());
        assert!(selected.bottom() < heatmap.top());
        assert_eq!(selected.left(), heatmap.left());
        assert_eq!(selected.size.width, heatmap.size.width);
    }
}

impl InputDriver {
    fn seed_cpu_layout_readings(&mut self) {
        self.seed_cpu_readings();
    }

    /// Gives the CPU page readings of 18 logical CPUs, 12 performance and 6 super, each 67% busy.
    pub(super) fn seed_cpu_readings(&mut self) {
        let cpus = 18;
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let levels = json!([{"name": "Super", "logical_cpus": 6},
                {"name": "Performance", "logical_cpus": 12}]);
            snapshot.selection.legacy_mut().unwrap()["trends"][0]["performance_levels"] =
                levels.clone();
            snapshot.selection.legacy_mut().unwrap()["trends"][1]["performance_levels"] =
                levels.clone();
            snapshot.selection.legacy_mut().unwrap()["summary"]["performance_levels"] = levels;
            snapshot.selection.legacy_mut().unwrap()["context_seconds"] = json!(900);
            snapshot.selection.legacy_mut().unwrap()["trends"][0]["run_tick"] = json!(1);
            snapshot.selection.legacy_mut().unwrap()["trends"][1]["run_tick"] = json!(2);
            snapshot.selection.legacy_mut().unwrap()["trends"][0]["cpu_percent"] = json!(13);
            snapshot.selection.legacy_mut().unwrap()["trends"][1]["cpu_percent"] = json!(67);
            let previous = snapshot.selection["trends"][0].clone();
            let selected = snapshot.selection["trends"][1].clone();
            snapshot.selection.legacy_mut().unwrap()["selected"] = selected.clone();
            snapshot.cpu_detail = Some(crate::live_source::Presentation::Legacy(
                crate::live_cpu::CPUActivity::decode(
                    json!({"observation":selected,"previous_observation":previous,
                    "interval_available":true,"unavailable_reason":null,
                    "cpus":(0..cpus).map(|cpu| json!({"cpu":cpu,"busy_percent":67}))
                        .collect::<Vec<_>>()}),
                    &selected,
                )
                .unwrap(),
            ));
            snapshot.cpu_history = Some(std::sync::Arc::new(
                crate::live_source::Presentation::Shared(Arc::new(
                    crate::live_cpu_history::CPUHistory::decode(
                        json!({"observation":selected,"context_seconds":900,
                    "first_ns":101_000_000_000_u64,"end_ns":1_001_000_000_000_u64,
                    "offset":0,"total":cpus,"retention_limited":false,
                    "observations":[
                        {"observation":previous,"previous_observation":null,
                            "interval_available":false,"cpu_count":cpus},
                        {"observation":selected,"previous_observation":previous,
                            "interval_available":true,"cpu_count":cpus}],
                    "cpus":(0..cpus).map(|cpu| json!({"cpu":cpu,"busy_percent":[null,67]}))
                        .collect::<Vec<_>>()}),
                        &snapshot.selection,
                    )
                    .unwrap(),
                )),
            ));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }
}
