use gpui::{TestAppContext, px};
use serde_json::{Value, json};

use super::{InputDriver, InputHarness, PlotExit, accepted_snapshot};
use crate::live_source::Update;

#[gpui::test]
fn schematic_battery_hover_keeps_historical_values_through_loading_and_exit(
    cx: &mut TestAppContext,
) {
    // Goal: the actual schematic projection follows installed plot hover and exact completion.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness
        .driver
        .seed_schematic_battery(json!({"charge_percent":33,"power_watts":-3}));
    assert_eq!(
        harness.driver.schematic_battery().unwrap(),
        [Some(88.0), Some(8.0)]
    );

    harness.driver.hover_previous_second_on("live-cpu-plot");
    let generation = harness.driver.begin_schematic_lookup();

    assert_eq!(
        harness.driver.schematic_battery().unwrap(),
        [Some(33.0), Some(-3.0)]
    );
    harness.driver.complete_schematic_lookup(generation);
    assert_eq!(
        harness.driver.schematic_battery().unwrap(),
        [Some(33.0), Some(-3.0)]
    );

    harness
        .driver
        .leave_plot("live-cpu-plot", PlotExit::Sidebar);

    assert_eq!(
        harness.driver.schematic_battery().unwrap(),
        [Some(88.0), Some(8.0)]
    );
    harness.teardown();
}

#[gpui::test]
fn schematic_battery_missing_hover_stays_unavailable_after_completion(cx: &mut TestAppContext) {
    // Goal: a real historical observation with no Battery cannot borrow current live values.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_schematic_battery(Value::Null);

    harness.driver.hover_previous_second_on("live-cpu-plot");
    let generation = harness.driver.begin_schematic_lookup();

    assert_eq!(harness.driver.schematic_battery().unwrap(), [None, None]);
    harness.driver.complete_schematic_lookup(generation);
    assert_eq!(harness.driver.schematic_battery().unwrap(), [None, None]);

    harness
        .driver
        .leave_plot("live-cpu-plot", PlotExit::Sidebar);

    assert_eq!(
        harness.driver.schematic_battery().unwrap(),
        [Some(88.0), Some(8.0)]
    );
    harness.teardown();
}

#[gpui::test]
fn schematic_battery_missing_hover_stays_unavailable_after_failure(cx: &mut TestAppContext) {
    // Goal: a failed exact lookup retains the missing resident Battery until pointer exit.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_schematic_battery(Value::Null);

    harness.driver.hover_previous_second_on("live-cpu-plot");
    let generation = harness.driver.begin_schematic_lookup();

    assert_eq!(harness.driver.schematic_battery().unwrap(), [None, None]);
    harness.driver.fail_schematic_lookup(generation);
    assert_eq!(harness.driver.schematic_battery().unwrap(), [None, None]);

    harness
        .driver
        .leave_plot("live-cpu-plot", PlotExit::Sidebar);

    assert_eq!(
        harness.driver.schematic_battery().unwrap(),
        [Some(88.0), Some(8.0)]
    );
    harness.teardown();
}

#[gpui::test]
fn schematic_battery_keeps_the_selected_same_second_publication(cx: &mut TestAppContext) {
    // Goal: a later same-second sibling cannot replace the explicitly selected Battery.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness
        .driver
        .seed_schematic_battery(json!({"charge_percent":33,"power_watts":-3}));

    harness.driver.hover_selected_second_on("live-cpu-plot");

    assert_eq!(
        harness.driver.schematic_battery().unwrap(),
        [Some(88.0), Some(8.0)]
    );
    assert_eq!(harness.driver.schematic_reading_publication(), Some(2));
    harness.teardown();
}

impl InputDriver {
    fn seed_schematic_battery(&mut self, historical_battery: Value) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let mut historical = snapshot.selection["trends"][0].clone();
            historical["cpu_percent"] = json!(13);
            historical["battery"] = historical_battery;
            let mut current = snapshot.selection["trends"][1].clone();
            current["cpu_percent"] = json!(87);
            current["battery"] = json!({"charge_percent":88,"power_watts":8});
            let mut sibling = current.clone();
            sibling["publication"] = json!(3);
            sibling["wall_ns"] = json!(1_000_900_000_000_u64);
            sibling["battery"] = json!({"charge_percent":99,"power_watts":9});
            snapshot.selection.legacy_mut().unwrap()["summary"] = current.clone();
            snapshot.selection.legacy_mut().unwrap()["trends"] =
                json!([historical, current, sibling]);
            snapshot.catalog["path"] = json!("schematic-battery-input.recording");
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        // The window holds the whole Overview, so the schematic under its last section is drawn.
        self.context
            .simulate_resize(gpui::size(px(1536.0), px(5000.0)));
        self.context.run_until_parked();
    }

    fn schematic_battery(&self) -> Result<[Option<f32>; 2], String> {
        self.view.read_with(&self.context, |dashboard, _| {
            let (readings, error) = dashboard.live_hardware_projection([None, None]);

            match error {
                Some(error) => Err(error),
                None => Ok(readings.battery),
            }
        })
    }

    fn schematic_reading_publication(&self) -> Option<u64> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.reading_snapshot()?.selection["selected"]["publication"].as_u64()
        })
    }

    fn begin_schematic_lookup(&mut self) -> u64 {
        let generation = self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
            let preview = dashboard.preview.as_ref().unwrap();
            assert!(!preview.complete);

            preview.generation.unwrap()
        });
        self.context.run_until_parked();

        generation
    }

    fn complete_schematic_lookup(&mut self, generation: u64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let preview = dashboard.preview.as_ref().unwrap();
            let mut snapshot = accepted_snapshot(generation);
            snapshot.catalog["path"] = preview.reading.catalog["path"].clone();
            snapshot.selection.legacy_mut().unwrap()["second"] = json!(preview.target.second);
            snapshot.selection.legacy_mut().unwrap()["selected"] =
                preview.target.identity.clone().unwrap();
            snapshot.selection.legacy_mut().unwrap()["summary"] =
                preview.reading.selection["summary"].clone();
            snapshot.selection.legacy_mut().unwrap()["trends"] =
                json!([snapshot.selection["summary"]]);
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn fail_schematic_lookup(&mut self, generation: u64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            assert!(dashboard.preview_receive(Update::Failed {
                generation,
                message: "Schematic exact details failed".into(),
            }));
            cx.notify();
        });
        self.context.run_until_parked();
    }
}

#[gpui::test]
fn malformed_battery_keeps_other_schematic_readings_and_a_rendered_error(cx: &mut TestAppContext) {
    // Goal: malformed Battery data reports its error without blanking unrelated HUD values.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_schematic_sensor_context(json!({
        "charge_percent":"broken","power_watts":4
    }));

    harness.driver.hover_previous_second_on("live-cpu-plot");

    harness
        .driver
        .assert_schematic_context_survives_battery_error();
    harness.teardown();
}

#[gpui::test]
fn nonfinite_battery_keeps_other_schematic_readings_and_a_rendered_error(cx: &mut TestAppContext) {
    // Goal: nonfinite Battery power leaves valid fan, utilization and memory readings intact.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_schematic_sensor_context(json!({
        "charge_percent":50,"power_watts":"NaN"
    }));

    harness.driver.hover_previous_second_on("live-cpu-plot");

    harness
        .driver
        .assert_schematic_context_survives_battery_error();
    harness.teardown();
}

#[gpui::test]
fn schematic_battery_populated_hover_keeps_history_after_exact_failure(cx: &mut TestAppContext) {
    // Goal: failed exact details retain the populated resident Battery until pointer exit.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness
        .driver
        .seed_schematic_battery(json!({"charge_percent":33,"power_watts":-3}));

    harness.driver.hover_previous_second_on("live-cpu-plot");
    let generation = harness.driver.begin_schematic_lookup();
    harness.driver.fail_schematic_lookup(generation);

    assert_eq!(
        harness.driver.schematic_battery().unwrap(),
        [Some(33.0), Some(-3.0)]
    );

    harness
        .driver
        .leave_plot("live-cpu-plot", PlotExit::Sidebar);

    assert_eq!(
        harness.driver.schematic_battery().unwrap(),
        [Some(88.0), Some(8.0)]
    );
    harness.teardown();
}

impl InputDriver {
    fn seed_schematic_sensor_context(&mut self, historical_battery: Value) {
        self.seed_schematic_battery(historical_battery);
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = dashboard.snapshot.as_mut().unwrap();
            for point in snapshot.selection.legacy_mut().unwrap()["trends"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .take(3)
            {
                point["gpu_active_percent"] = json!(37);
                point["memory_total_bytes"] = json!(34_359_738_368_u64);
                point["fan_rpm"] = json!([1500, 1600]);
            }
            snapshot.selection.legacy_mut().unwrap()["summary"] =
                snapshot.selection["trends"][1].clone();
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn assert_schematic_context_survives_battery_error(&mut self) {
        let (readings, error) = self.view.read_with(&self.context, |dashboard, _| {
            dashboard.live_hardware_projection([Some(1500.0), Some(1600.0)])
        });

        assert!(error.is_some());
        assert!(self.reading_is_visible("schematic-error"));
        assert_eq!(readings.speeds, [Some(1500.0), Some(1600.0)]);
        assert_eq!(readings.utilization, [Some(13.0), Some(37.0)]);
        assert_eq!(readings.memory, [None, Some(32.0)]);
        assert_eq!(readings.battery, [None, None]);
    }
}
