use gpui::{Bounds, Pixels, TestAppContext, px};
use serde_json::json;
use std::sync::Arc;

use super::{InputDriver, InputHarness, accepted_snapshot};

#[gpui::test]
fn cpu_composition_excludes_raw_tables_even_with_stale_pages(cx: &mut TestAppContext) {
    // Render stale raw payloads in a tall CPU window so an accidental table would be visible.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_cpu_layout_readings();
    harness.driver.seed_stale_cpu_table();

    assert!(!harness.driver.reading_is_visible("raw-heading:processes"));
    assert!(
        !harness
            .driver
            .reading_is_visible("raw-value:processes:0:123")
    );

    harness.teardown();
}

#[gpui::test]
fn raw_composition_displays_fan_rows(cx: &mut TestAppContext) {
    // Render an accepted fan page and inspect its actual heading and RPM cell geometry.
    let mut harness = InputHarness::setup_route(cx, "raw");
    harness.driver.seed_raw_fan_table();

    let heading = harness.driver.chart_bounds("raw-heading:fans");
    let value = harness.driver.chart_bounds("raw-value:fans:0:2345");
    assert!(heading.bottom() <= value.top());
    assert!(value.size.width > px(0.0));
    assert!(value.size.height > px(0.0));
    harness.teardown();
}

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

#[gpui::test]
fn compact_selected_cpu_bars_show_indices_and_restore_percentages_when_stacked(
    cx: &mut TestAppContext,
) {
    // Inspect the 18-CPU pane at its narrowest wide width, where percentage text cannot fit.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_cpu_layout_readings();
    harness.driver.resize_cpu_layout(1100.0);

    let label = harness
        .driver
        .context
        .debug_bounds("cpu-selected-value:0:67%");
    eprintln!("Wide CPU percentage label geometry: {label:?}");
    assert!(label.is_none());
    assert!(harness.driver.reading_is_visible("cpu-selected-value:0:0"));

    harness.driver.resize_cpu_layout(1099.0);
    assert!(
        harness
            .driver
            .reading_is_visible("cpu-selected-value:0:67%")
    );

    harness.teardown();
}

impl InputDriver {
    fn seed_stale_cpu_table(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.snapshot.as_mut().unwrap().pages.insert(
                "processes".into(),
                json!({"columns":["pid"],"rows":[[123]]}),
            );
            dashboard.rebuild();
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(4000.0)));
        self.context.run_until_parked();
    }

    fn seed_raw_fan_table(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.request.table = "fans".into();
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.pages.insert(
                "fans".into(),
                json!({"total":1,"offset":0,
                    "columns":["publication","fan","rpm"],"rows":[[2,0,2345]]}),
            );
            dashboard.accept(snapshot);
            cx.notify();
        });
        self.context.run_until_parked();
    }

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
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.selection["context_seconds"] = json!(900);
            snapshot.selection["trends"][0]["run_tick"] = json!(1);
            snapshot.selection["trends"][1]["run_tick"] = json!(2);
            snapshot.selection["trends"][0]["cpu_percent"] = json!(13);
            snapshot.selection["trends"][1]["cpu_percent"] = json!(67);
            let previous = snapshot.selection["trends"][0].clone();
            let selected = snapshot.selection["trends"][1].clone();
            snapshot.selection["selected"] = selected.clone();
            snapshot.cpu_detail = Some(
                crate::live_cpu::CPUActivity::decode(
                    json!({"observation":selected,"previous_observation":previous,
                    "interval_available":true,"unavailable_reason":null,
                    "cpus":(0..18).map(|cpu| json!({"cpu":cpu,"busy_percent":67}))
                        .collect::<Vec<_>>()}),
                    &selected,
                )
                .unwrap(),
            );
            snapshot.cpu_history = Some(Arc::new(
                crate::live_cpu_history::CPUHistory::decode(
                    json!({"observation":selected,"context_seconds":900,
                    "first_ns":101_000_000_000_u64,"end_ns":1_001_000_000_000_u64,
                    "offset":0,"total":18,"retention_limited":false,
                    "observations":[
                        {"observation":previous,"previous_observation":null,
                            "interval_available":false,"cpu_count":18},
                        {"observation":selected,"previous_observation":previous,
                            "interval_available":true,"cpu_count":18}],
                    "cpus":(0..18).map(|cpu| json!({"cpu":cpu,"busy_percent":[null,67]}))
                        .collect::<Vec<_>>()}),
                    &snapshot.selection,
                )
                .unwrap(),
            ));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }
}
