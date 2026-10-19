use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::json;

use super::{CPUHistory, NS};
use crate::live_dashboard::LiveDashboard;
use crate::live_source::{Snapshot, Worker};

#[gpui::test]
fn cpu_pointer_inspection_updates_the_page_to_its_exact_same_second_publication(
    cx: &mut TestAppContext,
) {
    // Dispatch the earlier interval while its sibling owns the committed scalar reading.
    let mut harness = CPUHoverHarness::setup(cx);
    assert!(harness.driver.reading_is_visible("87%"));

    harness.driver.hover_cpu(1000 * NS + NS * 9 / 10);

    assert!(harness.driver.reading_is_visible("13%"));
    assert!(harness.driver.cpu_reading_is_visible("publication 2", "0%"));
    assert_eq!(harness.driver.reading_publication(), Some(2));
    assert_eq!(harness.driver.committed_second(), None);
    harness.teardown();
}

#[gpui::test]
fn regular_pointer_inspection_synchronizes_the_cpu_history_readout(cx: &mut TestAppContext) {
    // Replace keyboard inspection with a regular graph's exact selected sibling.
    let mut harness = CPUHoverHarness::setup(cx);
    harness.driver.focus_cpu();
    harness.driver.press("left");
    assert!(harness.driver.cpu_reading_is_visible("publication 2", "0%"));

    harness.driver.hover_scalar_selected_second();

    assert!(harness.driver.reading_is_visible("87%"));
    assert!(
        harness
            .driver
            .cpu_reading_is_visible("publication 3", "50%")
    );
    assert_eq!(harness.driver.reading_publication(), Some(3));
    harness.teardown();
}

#[gpui::test]
fn cpu_interval_gaps_preserve_valid_scalars_without_borrowing_cpu_activity(
    cx: &mut TestAppContext,
) {
    // The final second contains a scalar observation but has no measured interval at this x.
    let mut harness = CPUHoverHarness::setup(cx);
    harness.driver.show_stacked_cpu_activity();
    assert!(harness.driver.reading_is_visible("73%"));
    assert!(
        !harness
            .driver
            .reading_is_visible("Logical CPU readings unavailable.")
    );

    harness.driver.hover_cpu(1001 * NS + NS * 3 / 4);

    assert!(harness.driver.reading_is_visible("87%"));
    assert!(
        harness
            .driver
            .reading_is_visible("Logical CPU readings unavailable.")
    );
    assert!(!harness.driver.reading_is_visible("73%"));
    assert!(
        harness
            .driver
            .reading_is_visible("No measurement interval here.")
    );
    assert_eq!(harness.driver.reading_publication(), Some(3));
    assert_eq!(harness.driver.committed_second(), None);
    harness.teardown();
}

#[gpui::test]
fn cpu_pointer_exit_into_the_row_labels_restores_the_committed_reading(cx: &mut TestAppContext) {
    // The adjacent labels belong to the panel but lie outside the painted cell area.
    let mut harness = CPUHoverHarness::setup(cx);
    harness.driver.hover_cpu(1000 * NS + NS * 9 / 10);
    assert!(harness.driver.reading_is_visible("13%"));

    harness.driver.hover_cpu_labels();

    assert!(harness.driver.reading_is_visible("87%"));
    assert!(harness.driver.target_is_clear());
    harness.teardown();
}

#[gpui::test]
fn cpu_empty_seconds_show_unavailable_readings_instead_of_the_committed_observation(
    cx: &mut TestAppContext,
) {
    // A point before the first observation still owns a timestamp inside the anchored domain.
    let mut harness = CPUHoverHarness::setup(cx);

    harness.driver.hover_cpu(999 * NS + NS / 2);

    assert!(harness.driver.reading_is_visible("Unavailable"));
    assert!(
        harness
            .driver
            .reading_is_visible("No measurement interval here.")
    );
    assert_eq!(harness.driver.reading_publication(), None);
    assert_eq!(harness.driver.committed_second(), None);
    harness.teardown();
}

#[gpui::test]
fn cpu_keyboard_inspection_survives_pointer_exit_and_return_commits_its_identity(
    cx: &mut TestAppContext,
) {
    // Focus and step through production input, then leave without replacing the keyboard owner.
    let mut harness = CPUHoverHarness::setup(cx);
    harness.driver.focus_cpu();
    harness.driver.press("left");

    harness.driver.hover_cpu_labels();

    assert!(harness.driver.reading_is_visible("13%"));
    assert_eq!(harness.driver.reading_publication(), Some(2));
    harness.driver.press("enter");
    assert_eq!(harness.driver.committed_second(), Some(1001));
    assert_eq!(harness.driver.committed_publication(), Some(2));
    harness.teardown();
}

struct CPUHoverHarness {
    driver: CPUHoverDriver,
}

impl CPUHoverHarness {
    fn setup(cx: &mut TestAppContext) -> Self {
        let worker = Worker::start(std::env::temp_dir().join(format!(
            "flightdeck-cpu-hover-contract-{}",
            std::process::id()
        )))
        .unwrap();
        let window = cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |_, cx| {
                crate::native_workflows::create_dashboard(worker, "cpu".into(), None, cx)
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let context = VisualTestContext::from_window(window.into(), cx);
        let mut harness = Self {
            driver: CPUHoverDriver { view, context },
        };
        harness.driver.seed_same_second_history();
        harness
    }

    fn teardown(self) {
        drop(self);
    }
}

impl Drop for CPUHoverHarness {
    fn drop(&mut self) {
        self.driver.context.update(|window, cx| {
            self.driver
                .view
                .update(cx, |dashboard, _| dashboard.worker.stop());
            window.remove_window();
        });
        self.driver.context.run_until_parked();
    }
}

struct CPUHoverDriver {
    view: Entity<LiveDashboard>,
    context: VisualTestContext,
}

impl CPUHoverDriver {
    fn show_stacked_cpu_activity(&mut self) {
        self.context
            .simulate_resize(gpui::size(gpui::px(1099.0), gpui::px(2400.0)));
        self.context.run_until_parked();
    }

    fn seed_same_second_history(&mut self) {
        let (mut payload, mut selection) = super::tests::fixture();
        let mut selected = payload["observation"].clone();
        selected["publication"] = json!(3);
        selected["run_tick"] = json!(3);
        selected["wall_ns"] = json!(1001 * NS + NS * 2 / 5);
        let previous = payload["observation"].clone();
        payload["observations"].as_array_mut().unwrap().push(json!({
            "observation":selected,"previous_observation":previous,
            "interval_available":true,"cpu_count":1}));
        payload["observation"] = selected.clone();
        payload["cpus"][0]["busy_percent"] = json!([null, 0.0, 50.0]);
        selection["selected"] = selected.clone();
        selection["trends"]
            .as_array_mut()
            .unwrap()
            .push(selected.clone());
        selection["trends"][0]["cpu_percent"] = json!(7);
        selection["trends"][1]["cpu_percent"] = json!(13);
        selection["trends"][2]["cpu_percent"] = json!(87);
        selection["summary"] = selection["trends"][2].clone();
        let activity = crate::live_cpu::CPUActivity::decode(
            json!({
                "observation":selected, "previous_observation":previous,
                "interval_available":true, "unavailable_reason":null,
                "cpus":[{"cpu":0,"busy_percent":73.0}]
            }),
            &selected,
        )
        .unwrap();
        let history = Arc::new(CPUHistory::decode(payload, &selection).unwrap());
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.accept(Box::new(Snapshot {
                generation: dashboard.generation,
                catalog: json!({"first_second":1000,"last_second":1001}),
                selection,
                pages: BTreeMap::new(),
                apps: None,
                detail: None,
                history: None,
                cpu_detail: Some(activity),
                cpu_history: Some(history),
                resident_history: None,
                app_attribution_history: None,
                overview_app_histories: None,
                histories_pending: false,
                process_history: None,
                rankings: None,
                read_ms: 0.0,
            }));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(1600.0)));
        self.context.run_until_parked();
    }

    fn hover_cpu(&mut self, wall_ns: u64) {
        let bounds = self.context.debug_bounds("cpu-history-cells").unwrap();
        let fraction = self.view.read_with(&self.context, |dashboard, _| {
            dashboard
                .snapshot
                .as_ref()
                .unwrap()
                .cpu_history
                .as_ref()
                .unwrap()
                .fraction(wall_ns)
        });
        let fraction = num_traits::ToPrimitive::to_f32(&fraction).unwrap();
        self.hover_position(gpui::point(
            bounds.left() + bounds.size.width * fraction,
            bounds.top() + gpui::px(8.0),
        ));
    }

    fn hover_scalar_selected_second(&mut self) {
        let bounds = self.context.debug_bounds("live-cpu-plot").unwrap();
        let plot = crate::charts::plot_bounds(bounds);
        self.hover_position(gpui::point(
            plot.left() + plot.size.width * (899.5 / 900.0),
            plot.center().y,
        ));
    }

    fn hover_cpu_labels(&mut self) {
        let bounds = self.context.debug_bounds("cpu-history-cells").unwrap();
        self.hover_position(gpui::point(
            bounds.left() - gpui::px(10.0),
            bounds.center().y,
        ));
    }

    fn hover_position(&mut self, position: gpui::Point<gpui::Pixels>) {
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
    }

    fn focus_cpu(&mut self) {
        let bounds = self.context.debug_bounds("cpu-history-plot").unwrap();
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
    }

    fn press(&mut self, key: &str) {
        self.context.simulate_keystrokes(key);
    }

    fn reading_is_visible(&mut self, reading: &'static str) -> bool {
        self.context.debug_bounds(reading).is_some()
    }

    fn cpu_reading_is_visible(&mut self, publication: &'static str, value: &'static str) -> bool {
        self.reading_is_visible("Logical CPU 0")
            && self.reading_is_visible(value)
            && self.reading_is_visible(publication)
    }

    fn reading_publication(&self) -> Option<u64> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.reading_snapshot()?.selection["selected"]["publication"].as_u64()
        })
    }

    fn committed_second(&self) -> Option<u64> {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.request.second)
    }

    fn committed_publication(&self) -> Option<u64> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.request.identity.as_ref()?["publication"].as_u64()
        })
    }

    fn target_is_clear(&self) -> bool {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.reading_target().is_none()
        })
    }
}
