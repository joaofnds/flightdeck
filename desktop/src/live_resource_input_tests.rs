use std::sync::Arc;

use gpui::{TestAppContext, px, size};
use serde_json::{Value, json};
use viewer::store::MachineSecond;

use super::{InputDriver, InputHarness, PlotExit, accepted_snapshot};
use crate::live_store_app_fixtures::apps_at;
use crate::live_store_fixtures::{
    battery_reading, reading_at, summary_point, view, with_battery_readings,
};
use crate::live_store_page_fixtures::{readings, texts};
use crate::live_views::summary;

#[gpui::test]
fn battery_hover_updates_selected_details_charging_metadata_and_copy_then_restores(
    cx: &mut TestAppContext,
) {
    // Goal: every Battery readout and Copy use the same hovered publication as the graph.
    // Method: move over a repeated second, activate the focused Copy control, then leave.
    let mut harness = InputHarness::setup_route(cx, "battery");
    harness.driver.seed_resources(false);
    assert!(harness.driver.reading_is_visible("summary-card:Charge:88%"));

    harness
        .driver
        .hover_previous_second_on("live-battery-charge-plot");

    for selector in [
        "summary-card:Charge:33%",
        "summary-card:Battery flow:-3 W",
        "summary-card:Health:104%",
        "overview-context:cycles:30",
    ] {
        assert!(harness.driver.reading_is_visible(selector), "{selector}");
    }
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

    assert!(harness.driver.reading_is_visible("summary-card:Charge:88%"));
    assert!(!harness.driver.reading_is_visible("summary-card:Charge:33%"));
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

    harness
        .driver
        .hover_selected_second_on("live-battery-charge-plot");

    assert!(harness.driver.reading_is_visible("summary-card:Charge:88%"));
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

mod when_the_page_is_drawn_from_the_view {
    use super::*;

    #[gpui::test]
    fn the_cards_and_details_read_the_newest_seconds_battery(cx: &mut TestAppContext) {
        // Goal: a followed Battery page drawn from the store's View shows the cards and details
        // of the View's newest second as the reader's cards show the same complete reading, a
        // measured "not charging" among them, as
        // `battery_cards_preserve_signed_flow_false_and_exact_cycles` pins the reader's. Method:
        // the page follows a View of four seconds whose newest does not charge, over the
        // worker's readings of the first two, and its cards and the battery the details read.
        let seconds = view_battery_seconds();
        let mut harness = InputHarness::open_route(cx, "battery");
        harness.driver.show_battery_view(&seconds);

        let cards = harness.driver.readout_texts();

        assert_eq!(cards, battery_cards_of(&seconds[3]));
        assert!(cards.contains(&card("Charging", "No")));
        assert_eq!(
            harness.driver.shown_battery(),
            summary_point(&seconds[3])["battery"]
        );
        harness.teardown();
    }

    #[gpui::test]
    fn a_hover_shows_the_hovered_seconds_battery_and_copy_then_restores(cx: &mut TestAppContext) {
        // Goal: a hover over the Battery page drawn from the View shows the hovered second's
        // cards, details and charging state, and Copy names that second and its publication, a
        // second the worker has not read, and the page shows the newest second's once the
        // pointer leaves, as
        // `battery_hover_updates_selected_details_charging_metadata_and_copy_then_restores`
        // pins the reader's. Method: the pointer rests on 10,003, the one second that charges,
        // then leaves the plot.
        let seconds = view_battery_seconds();
        let mut harness = InputHarness::open_route(cx, "battery");
        harness.driver.show_battery_view(&seconds);

        harness.driver.hover_battery_second(10_003);

        assert_eq!(
            harness.driver.readout_texts(),
            battery_cards_of(&seconds[2])
        );
        assert_eq!(
            harness.driver.shown_battery(),
            summary_point(&seconds[2])["battery"]
        );
        let (second, reading) = harness
            .driver
            .chart_accessibility_reading("live-battery-charging");
        assert_eq!(second, Some(10_003));
        assert!(reading.ends_with("; Charging"), "{reading}");
        assert!(
            harness
                .driver
                .reading_is_visible("chart-caption:live-battery-charging:Charging")
        );
        let copied = harness.driver.activate_resource_copy();
        assert_eq!(copied["second"], 10_003);
        assert_eq!(copied["observation"]["publication"], 10_003);

        harness
            .driver
            .leave_plot("live-battery-charge-plot", PlotExit::Sidebar);

        assert_eq!(
            harness.driver.readout_texts(),
            battery_cards_of(&seconds[3])
        );
        assert!(
            harness
                .driver
                .reading_is_visible("chart-caption:live-battery-charging:Not charging")
        );
        harness.teardown();
    }

    #[gpui::test]
    fn a_hover_over_a_second_the_view_lacks_clears_the_battery_and_copy(cx: &mut TestAppContext) {
        // Goal: a hover over a second of the Battery page drawn from the View that the View
        // holds no reading of shows no battery, charging state or copied observation, rather
        // than another second's, as `battery_gap_clears_selected_details_charging_and_copied_
        // observation` pins the reader's. Method: the pointer rests on 10,003, then on 10,004,
        // which the View lacks.
        let seconds = view_battery_seconds();
        let mut harness = InputHarness::open_route(cx, "battery");
        harness.driver.show_battery_view(&seconds);
        harness.driver.hover_battery_second(10_003);

        harness.driver.hover_battery_second(10_004);

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
        assert_eq!(second, Some(10_004));
        assert!(reading.ends_with("Unavailable"), "{reading}");
        let copied = harness.driver.activate_resource_copy();
        assert_eq!(copied["second"], 10_004);
        assert!(copied["observation"].is_null());
        assert!(copied["summary"].is_null());
        harness.teardown();
    }

    #[gpui::test]
    fn the_charging_timeline_paints_a_cell_for_each_second_the_view_holds(cx: &mut TestAppContext) {
        // Goal: the charging timeline drawn from the View paints a cell for each second it holds
        // a charging state of and leaves every other second blank, and its caption names the
        // newest second's state, as
        // `charging_timeline_paints_only_observed_cells_and_names_the_selected_state` pins the
        // reader's. Method: the View's four seconds, no two of them next to each other, drawn at
        // twice the scale, and the state-cell quads the scene paints.
        let seconds = view_battery_seconds();
        let mut harness = InputHarness::open_route(cx, "battery");
        harness.driver.show_battery_view(&seconds);
        harness.driver.context.simulate_scale_factor_change(2.0);
        harness.driver.context.run_until_parked();

        let cells = harness
            .driver
            .resource_timeline_cell_count("live-battery-charging-plot");

        assert_eq!(cells, seconds.len());
        assert!(
            harness
                .driver
                .reading_is_visible("chart-caption:live-battery-charging:Not charging")
        );
        harness.teardown();
    }

    /// The seconds 9,997, 10,000, 10,003 and 10,005, no two of them next to each other, which
    /// the View holds and of which the worker reads the first two, each with a battery that holds
    /// its own charge and power, and that charges at 10,003 alone.
    fn view_battery_seconds() -> [MachineSecond; 4] {
        [9_997, 10_000, 10_003, 10_005].map(|wall_second| {
            let second = reading_at(wall_second, 0);
            let mut battery = battery_reading(&second);
            battery.charging = Some(wall_second == 10_003);
            with_battery_readings(second, Some(battery))
        })
    }

    /// The Battery page's cards as the reader shows them for its reading of `second`, the trend
    /// point the viewer writes of the second's complete reading.
    fn battery_cards_of(second: &MachineSecond) -> Vec<(String, String, String, u32)> {
        texts(&summary::cards_of_summary(
            "battery",
            &summary_point(second),
        ))
    }

    /// The card `label`, which reads `reading`, as the Battery page's cards read.
    fn card(label: &str, reading: &str) -> (String, String, String, u32) {
        let cards = summary::cards_of_summary(
            "battery",
            &json!({"battery": {"charging": reading == "Yes"}}),
        );
        let found = texts(&cards)
            .into_iter()
            .find(|(found, ..)| found == label)
            .unwrap();
        assert_eq!(found.1, reading);
        found
    }

    impl InputDriver {
        /// Shows the Battery page following the View of `seconds`, over the worker's readings of
        /// the first two.
        fn show_battery_view(&mut self, seconds: &[MachineSecond]) {
            let snapshot = readings(&seconds[..2]);
            let mut held = view(1, &[], seconds);
            held.apps_loaded = true;
            held.apps = seconds
                .iter()
                .map(|second| Arc::new(apps_at(second, &[("A", 1_000)])))
                .collect();
            self.view.update(&mut self.context, |dashboard, cx| {
                dashboard.accept(snapshot);
                assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
                assert!(dashboard.take_view(Arc::new(held)));
                cx.notify();
            });
            self.context.simulate_resize(size(px(1536.0), px(3600.0)));
            self.context.run_until_parked();
        }

        /// Rests the pointer on `second` of the charge chart, where the range shows it.
        fn hover_battery_second(&mut self, second: u64) {
            let window = self
                .view
                .read_with(&self.context, |dashboard, _| dashboard.view_window());
            let fraction = window.unwrap().fraction_of(second).unwrap();
            let position = self.second_position("live-battery-charge-plot", fraction);

            self.hover_position(position);
            self.context.run_until_parked();
        }

        /// The battery the page's cards and details read.
        fn shown_battery(&self) -> Value {
            self.view.read_with(&self.context, |dashboard, _| {
                dashboard.shown_summary()["battery"].clone()
            })
        }
    }
}

impl InputDriver {
    fn seed_resources(&mut self, later_selected_sibling: bool) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let template = snapshot.selection.trends_json()[1].clone();
            let earlier = resource_point(&template, ResourceEpoch::EarlierSibling);
            let historical = resource_point(&template, ResourceEpoch::Historical);
            let selected = resource_point(&template, ResourceEpoch::Current);
            let observation = json!({"run":selected["run"], "boot":selected["boot"],
                "publication":selected["publication"], "wall_ns":selected["wall_ns"]});
            snapshot.selection.legacy_mut().unwrap()["selected"] = observation.clone();
            snapshot.selection.legacy_mut().unwrap()["summary"] = selected.clone();
            snapshot
                .selection
                .legacy_mut()
                .unwrap()
                .trends_edit(|points| *points = json!([earlier, historical, selected]));
            snapshot.selection.legacy_mut().unwrap()["alternatives"] = json!([observation.clone()]);
            snapshot.catalog["path"] = json!("resource-input-fixture.recording");
            if later_selected_sibling {
                let mut later = snapshot.selection.trends_json()[2].clone();
                later["publication"] = json!(5);
                later["wall_ns"] = json!(1_000_900_000_000_u64);
                later["battery"]["charging"] = json!(true);
                later["battery"]["charge_percent"] = json!(99);
                snapshot
                    .selection
                    .legacy_mut()
                    .unwrap()
                    .trends_edit(|points| points.as_array_mut().unwrap().push(later));
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

    /// The prototype has no copy control, so the copy is the model's own: the text the control
    /// copied while it existed.
    fn activate_resource_copy(&mut self) -> Value {
        let text = self
            .view
            .read_with(&self.context, |dashboard, _| {
                crate::native_workflows::selected_copy(
                    dashboard.reading_snapshot(),
                    dashboard.reading_request(),
                    None,
                )
            })
            .unwrap();
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
