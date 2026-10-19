use gpui::{Modifiers, MouseButton, TestAppContext, px, size};
use serde_json::{Value, json};

use super::{InputDriver, InputHarness, PlotExit, accepted_snapshot};

#[gpui::test]
fn battery_hover_updates_selected_details_charging_metadata_and_copy_then_restores(
    cx: &mut TestAppContext,
) {
    // Goal: every Battery readout and Copy use the same hovered publication as the graph.
    // Method: move over a repeated second, activate the focused Copy control, then leave.
    let mut harness = InputHarness::setup_route(cx, "battery");
    harness.driver.seed_resources(false);
    harness.driver.focus_resource_copy();
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Charge:88.0 %")
    );

    harness
        .driver
        .hover_previous_second_on("live-battery-charge-plot");

    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Charge:33.0 %")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Battery flow:-3.0 W")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Health:104.0 %")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("overview-context:cycles:30")
    );
    let (second, reading) = harness
        .driver
        .chart_accessibility_reading("live-battery-charging");
    assert_eq!(second, Some(999));
    assert!(reading.ends_with("Charging"));
    assert!(
        harness
            .driver
            .reading_is_visible("chart-caption:live-battery-charging:Charging")
    );
    let copied = harness.driver.activate_resource_copy();
    assert_eq!(copied["second"], 999);
    assert_eq!(copied["observation"]["publication"], 3);
    assert_eq!(copied["summary"]["wall_ns"], 999_750_000_000_u64);
    assert_eq!(copied["summary"]["battery"]["charge_percent"], 33.0);

    harness
        .driver
        .leave_plot("live-battery-charge-plot", PlotExit::Sidebar);

    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Charge:88.0 %")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("summary-card:Charge:33.0 %")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("chart-caption:live-battery-charging:Not charging")
    );
    harness.teardown();
}

#[gpui::test]
fn battery_gap_clears_selected_details_charging_and_copied_observation(cx: &mut TestAppContext) {
    // Goal: an unobserved second cannot reuse the prior Battery details or charging state.
    // Method: move from a measured historical second into a true gap through rendered input.
    let mut harness = InputHarness::setup_route(cx, "battery");
    harness.driver.seed_resources(false);
    harness.driver.focus_resource_copy();
    harness
        .driver
        .hover_previous_second_on("live-battery-charge-plot");

    let gap = harness
        .driver
        .second_position("live-battery-charge-plot", 897.5 / 900.0);
    harness.driver.hover_position(gap);

    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Charge:Unavailable")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Charging:Unavailable")
    );
    let (second, reading) = harness
        .driver
        .chart_accessibility_reading("live-battery-charging");
    assert_eq!(second, Some(998));
    assert!(reading.ends_with("Unavailable"));
    let copied = harness.driver.activate_resource_copy();
    assert_eq!(copied["second"], 998);
    assert!(copied["observation"].is_null());
    assert!(copied["summary"].is_null());
    harness.teardown();
}

#[gpui::test]
fn storage_hover_swaps_virtual_device_rows_and_totals_without_moving_the_plot(
    cx: &mut TestAppContext,
) {
    // Goal: Storage totals, device rows and Copy share one historical publication.
    // Method: hover a two-device observation from a one-device selection and inspect layout.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_resources(false);
    harness.driver.focus_resource_copy();
    let plot = harness.driver.plot_bounds("live-storage-throughput-plot");
    harness
        .driver
        .assert_resource_visible("resource-device:current-disk");

    harness
        .driver
        .hover_previous_second_on("live-storage-throughput-plot");

    assert_eq!(
        harness.driver.plot_bounds("live-storage-throughput-plot"),
        plot
    );
    harness
        .driver
        .assert_resource_visible("summary-card:Disk read:3.0 MB/s");
    harness
        .driver
        .assert_resource_visible("summary-card:Disk write:1.5 MB/s");
    harness
        .driver
        .assert_resource_visible("resource-device:historical-disk0");
    harness
        .driver
        .assert_resource_visible("resource-device:historical-disk1");
    assert!(
        !harness
            .driver
            .reading_is_visible("resource-device:current-disk")
    );
    let copied = harness.driver.activate_resource_copy();
    assert_eq!(copied["observation"]["publication"], 3);
    assert_eq!(
        copied["summary"]["storage"]["devices"][1]["name"],
        "historical-disk1"
    );

    harness
        .driver
        .leave_plot("live-storage-throughput-plot", PlotExit::Sidebar);

    harness
        .driver
        .assert_resource_visible("summary-card:Disk read:8.0 MB/s");
    harness
        .driver
        .assert_resource_visible("resource-device:current-disk");
    assert!(
        !harness
            .driver
            .reading_is_visible("resource-device:historical-disk1")
    );
    harness.teardown();
}

#[gpui::test]
fn charging_timeline_paints_only_observed_cells_and_names_the_selected_state(
    cx: &mut TestAppContext,
) {
    // Goal: sparse charging history leaves unobserved seconds blank in the composed scene.
    // Method: count actual painted state-cell quads and read the compact timeline's caption.
    let mut harness = InputHarness::setup_route(cx, "battery");
    harness.driver.seed_resources(false);
    harness.driver.context.simulate_scale_factor_change(2.0);
    harness.driver.context.run_until_parked();

    let cells = harness
        .driver
        .resource_timeline_cell_count("live-battery-charging-plot");

    assert_eq!(cells, 2);
    assert!(
        harness
            .driver
            .reading_is_visible("chart-caption:live-battery-charging:Not charging")
    );
    harness.teardown();
}

#[gpui::test]
fn charging_hover_retains_an_explicitly_selected_earlier_same_second_publication(
    cx: &mut TestAppContext,
) {
    // Goal: a later same-second publication cannot overwrite the selected charging state.
    // Method: hover the selected second with a conflicting later sibling in resident history.
    let mut harness = InputHarness::setup_route(cx, "battery");
    harness.driver.seed_resources(true);
    harness.driver.focus_resource_copy();

    harness
        .driver
        .hover_selected_second_on("live-battery-charge-plot");

    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Charge:88.0 %")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Charging:No")
    );
    let (second, reading) = harness
        .driver
        .chart_accessibility_reading("live-battery-charging");
    assert_eq!(second, Some(1000));
    assert!(reading.ends_with("Not charging"));
    let copied = harness.driver.activate_resource_copy();
    assert_eq!(copied["observation"]["publication"], 4);
    assert_eq!(copied["summary"]["battery"]["charging"], false);
    harness.teardown();
}

impl InputDriver {
    fn assert_resource_visible(&mut self, selector: &'static str) {
        assert!(
            self.reading_is_visible(selector),
            "{selector} must be rendered"
        );
    }

    fn seed_resources(&mut self, later_selected_sibling: bool) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let template = snapshot.selection["trends"][1].clone();
            let earlier = resource_point(&template, ResourceEpoch::EarlierSibling);
            let historical = resource_point(&template, ResourceEpoch::Historical);
            let selected = resource_point(&template, ResourceEpoch::Current);
            let observation = json!({"run":selected["run"], "boot":selected["boot"],
                "publication":selected["publication"], "wall_ns":selected["wall_ns"]});
            snapshot.selection["selected"] = observation.clone();
            snapshot.selection["summary"] = selected.clone();
            snapshot.selection["trends"] = json!([earlier, historical, selected]);
            snapshot.selection["alternatives"] = json!([observation.clone()]);
            snapshot.catalog["path"] = json!("resource-input-fixture.recording");
            if later_selected_sibling {
                let mut later = snapshot.selection["trends"][2].clone();
                later["publication"] = json!(5);
                later["wall_ns"] = json!(1_000_900_000_000_u64);
                later["battery"]["charging"] = json!(true);
                later["battery"]["charge_percent"] = json!(99);
                snapshot.selection["trends"]
                    .as_array_mut()
                    .unwrap()
                    .push(later);
            }
            dashboard.request.second = Some(1000);
            dashboard.request.identity = Some(observation);
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.simulate_resize(size(px(1536.0), px(3600.0)));
        self.context.run_until_parked();
    }

    fn focus_resource_copy(&mut self) {
        let bounds = crate::live_dashboard::input_tests::menu_control_bounds(
            &mut self.context,
            "copy-reading",
        );
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
    }

    fn activate_resource_copy(&mut self) -> Value {
        self.context.simulate_keystrokes("space");
        self.context.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("space").unwrap(),
        });
        let text = self.context.read_from_clipboard().unwrap().text().unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn resource_timeline_cell_count(&mut self, selector: &'static str) -> usize {
        let plot = self.plot_bounds(selector);
        self.context.update(|window, _| {
            let scale = window.scale_factor();
            let width = (plot.size.width / 900.0).scale(scale).as_f32();
            assert!(width >= 1.0);
            let top = resource_device_pixel(plot.top().scale(scale).as_f32());
            let bottom = resource_device_pixel(plot.bottom().scale(scale).as_f32());
            let left = resource_device_pixel(plot.left().scale(scale).as_f32());
            let right = resource_device_pixel(plot.right().scale(scale).as_f32());
            let cells: Vec<_> = window
                .painted_quads()
                .into_iter()
                .filter(|quad| {
                    if quad.bounds.size.height != gpui::ScaledPixels(bottom - top) {
                        return false;
                    }
                    if quad.bounds.top() != gpui::ScaledPixels(top) {
                        return false;
                    }
                    if !(width.floor()..=width.ceil()).contains(&quad.bounds.size.width.as_f32()) {
                        return false;
                    }
                    (left..right).contains(&quad.bounds.left().as_f32())
                })
                .collect();
            cells.len()
        })
    }
}

fn resource_device_pixel(value: f32) -> f32 {
    // GPUI snaps both edges with util::round_half_toward_zero before storing scene quads.
    (value.abs() - 0.5).ceil().copysign(value)
}

#[derive(Clone, Copy)]
pub(super) enum ResourceEpoch {
    EarlierSibling,
    Historical,
    Current,
}

fn resource_point(template: &Value, epoch: ResourceEpoch) -> Value {
    let (publication, wall_ns, charge, flow, charging, health, rate) = match epoch {
        ResourceEpoch::EarlierSibling => (2_u64, 999_100_000_000_u64, 21.0, -2.0, false, 103, 1.0),
        ResourceEpoch::Historical => (3, 999_750_000_000, 33.0, -3.0, true, 104, 3.0),
        ResourceEpoch::Current => (4, 1_000_000_000_000, 88.0, 8.0, false, 99, 8.0),
    };
    let mut point = template.clone();
    point["publication"] = json!(publication);
    point["wall_ns"] = json!(wall_ns);
    point["battery"] = json!({"charge_percent":charge, "power_watts":flow,
        "charging":charging, "health_percent":health, "cycle_count":publication * 10,
        "adapter_watts":100, "external_power":charging, "age_seconds":0,
        "source_continuous_ns":wall_ns});
    point["storage"] = json!({"state":"complete", "read_bps":rate * 1_000_000.0,
        "write_bps":rate * 500_000.0, "read_ops_per_second":rate * 10.0,
        "write_ops_per_second":rate * 5.0, "read_errors_per_second":0,
        "write_errors_per_second":0, "read_mean_ms":0.004, "write_mean_ms":null,
        "devices":resource_devices(epoch)});
    point
}

pub(super) fn resource_devices(epoch: ResourceEpoch) -> Value {
    match epoch {
        ResourceEpoch::EarlierSibling => json!([
            {"driver_id":20, "media_id":1, "name":"earlier-disk", "read_bps":1_000_000}
        ]),
        ResourceEpoch::Historical => json!([
            {"driver_id":30, "media_id":1, "name":"historical-disk0", "read_bps":1_000_000},
            {"driver_id":31, "media_id":2, "name":"historical-disk1", "read_bps":2_000_000}
        ]),
        ResourceEpoch::Current => json!([
            {"driver_id":40, "media_id":1, "name":"current-disk", "read_bps":8_000_000}
        ]),
    }
}
