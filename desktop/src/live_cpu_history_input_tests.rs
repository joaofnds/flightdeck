use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::{Value, json};

use super::{CPUHistory, NS};
use crate::live_dashboard::LiveDashboard;
use crate::live_source::{Snapshot, Worker};

#[gpui::test]
fn keyboard_history_inspection_selects_the_exact_same_second_observation(cx: &mut TestAppContext) {
    // Goal: Return selects the inspected publication rather than its same-second sibling.
    // Method: dispatch Left and Return through the production CPU history element tree.
    let mut harness = CPUHistoryHarness::setup(cx);
    harness.driver.focus_plot();

    harness.driver.press("left");
    assert_eq!(harness.driver.inspected_publication(), Some(2));
    harness.driver.press("enter");

    assert_eq!(
        harness.driver.requested_selection(),
        (
            Some(1001),
            Some(json!({
        "run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004","publication":2}))
        )
    );
    assert!(harness.driver.readings_are_hidden());
    harness.teardown();
}

#[gpui::test]
fn clicking_a_history_interval_changes_nothing(cx: &mut TestAppContext) {
    // Goal: a plain click on the CPU history never requests another second, since only a drag
    // acts on a chart. Method: click a measured interval and a blank one, then read the request.
    let mut harness = CPUHistoryHarness::setup(cx);
    let previous = harness.driver.requested_selection();

    harness.driver.click_at(1000 * NS + NS * 9 / 10);
    harness.driver.click_at(1001 * NS + NS * 3 / 4);

    assert_eq!(harness.driver.requested_selection(), previous);
    assert!(!harness.driver.readings_are_hidden());
    harness.teardown();
}

#[gpui::test]
fn hovering_a_zoomed_history_inspects_the_interval_under_the_pointer(cx: &mut TestAppContext) {
    // Goal: once a range narrows the view, the pointer inspects the interval drawn under it
    // and not the one at the same fraction of the whole history.
    // Method: view only second 1001, hover a fifth across the cells, and read the publication.
    let mut harness = CPUHistoryHarness::setup(cx);
    harness.driver.view_only_second(1001);

    harness.driver.hover_cells(0.2);

    assert_eq!(harness.driver.inspected_publication(), Some(3));
    harness.teardown();
}

struct CPUHistoryHarness {
    driver: CPUHistoryDriver,
}

impl CPUHistoryHarness {
    fn setup(cx: &mut TestAppContext) -> Self {
        let worker = Worker::start(std::env::temp_dir().join(format!(
            "flightdeck-cpu-input-contract-{}",
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
            driver: CPUHistoryDriver { view, context },
        };
        harness.driver.seed_history();

        harness
    }

    fn teardown(self) {
        drop(self);
    }
}

impl Drop for CPUHistoryHarness {
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

struct CPUHistoryDriver {
    view: Entity<LiveDashboard>,
    context: VisualTestContext,
}

struct CPUHistoryFixture {
    payload: Value,
    selection: Value,
}

impl CPUHistoryDriver {
    fn seed_history(&mut self) {
        let (mut payload, mut selection) = super::tests::fixture();
        payload["observation"]["publication"] = json!(3);
        payload["observation"]["run_tick"] = json!(3);
        payload["observation"]["wall_ns"] = json!(1001 * NS + NS * 2 / 5);
        let selected = payload["observation"].clone();
        let previous = payload["observations"][1]["observation"].clone();
        payload["observations"].as_array_mut().unwrap().push(json!({
            "observation":selected,"previous_observation":previous,
            "interval_available":true,"cpu_count":1}));
        payload["cpus"][0]["busy_percent"]
            .as_array_mut()
            .unwrap()
            .push(json!(50.0));
        selection["selected"] = selected.clone();
        selection["trends"].as_array_mut().unwrap().push(selected);
        self.install_history(CPUHistoryFixture { payload, selection });
    }

    fn install_history(&mut self, fixture: CPUHistoryFixture) {
        let history = Arc::new(CPUHistory::decode(fixture.payload, &fixture.selection).unwrap());
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.request.second = Some(1001);
            dashboard.request.identity = Some(fixture.selection["selected"].clone());
            dashboard.accept(Box::new(Snapshot {
                generation: dashboard.generation,
                catalog: json!({"first_second":1000,"last_second":1001}),
                selection: fixture.selection,
                pages: BTreeMap::new(),
                apps: None,
                detail: None,
                history: None,
                cpu_detail: None,
                cpu_history: Some(history),
                resident_history: None,
                app_attribution_history: None,
                overview_app_histories: None,
                histories_pending: false,
                process_history: None,
                rankings: None,
                app_ranks: None,
                app_parts: None,
                read_ms: 0.0,
            }));
            cx.notify();
        });
        let error = self
            .view
            .read_with(&self.context, |dashboard, _| dashboard.error.clone());
        assert!(error.is_none(), "{error:?}");
        self.context.run_until_parked();
    }

    fn focus_plot(&mut self) {
        let bounds = self.context.debug_bounds("cpu-history-plot").unwrap();
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
    }

    fn press(&mut self, keys: &str) {
        self.context.simulate_keystrokes(keys);
    }

    fn click_at(&mut self, wall_ns: u64) {
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
        let position = gpui::point(
            bounds.origin.x + bounds.size.width * fraction,
            bounds.center().y,
        );
        self.context.simulate_click(position, Modifiers::none());
    }

    fn view_only_second(&mut self, second: u64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.range = crate::time_range::Range::Between {
                first: second,
                last: second,
            };
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn hover_cells(&mut self, fraction: f32) {
        let bounds = self.context.debug_bounds("cpu-history-cells").unwrap();
        let position = gpui::point(
            bounds.origin.x + bounds.size.width * fraction,
            bounds.center().y,
        );
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
        self.context.run_until_parked();
    }

    fn inspected_publication(&self) -> Option<u64> {
        self.view.read_with(&self.context, |dashboard, _| {
            let history = dashboard.snapshot.as_ref()?.cpu_history.as_ref()?;
            let inspection = dashboard.cpu_inspection.or_else(|| history.selected())?;
            history.target(inspection)?.1["publication"].as_u64()
        })
    }

    fn requested_selection(&self) -> (Option<u64>, Option<Value>) {
        self.view.read_with(&self.context, |dashboard, _| {
            (dashboard.request.second, dashboard.request.identity.clone())
        })
    }

    fn readings_are_hidden(&self) -> bool {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.snapshot.is_none())
    }
}
