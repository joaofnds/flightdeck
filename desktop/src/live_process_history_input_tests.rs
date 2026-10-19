use super::*;
use crate::live_process_history::ProcessHistory;
use crate::process_history_tests::payload;

#[path = "live_apps_layout_input_tests.rs"]
mod apps_layout;

#[gpui::test]
fn named_process_history_selection_loads_distinct_panels_before_app_totals_and_instances(
    cx: &mut TestAppContext,
) {
    // Select a real installed group row and inspect the composed order after its worker response.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.resize_process_history();

    harness.driver.choose_named_process();
    harness.driver.deliver_named_process_history();

    harness.driver.assert_named_process_order();
    assert!(
        harness
            .driver
            .text_is_visible("chart-reading:live-process-group-cpu:240.0 %")
    );
    assert!(
        harness
            .driver
            .text_is_visible("chart-reading:live-app-cpu:87.0 %")
    );
    harness.teardown();
}

#[gpui::test]
fn named_process_history_data_reports_captured_sample_availability(cx: &mut TestAppContext) {
    // Inspect the named chart's own samples independently of App totals and page layout.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.resize_process_history();
    harness.driver.choose_named_process();
    harness.driver.deliver_named_process_history();

    harness
        .driver
        .process_history_control("chart-data:live-process-group-cpu");

    assert!(harness.driver.text_is_visible("Available samples"));
    assert!(harness.driver.text_is_visible("2 / 2"));
    harness.teardown();
}

#[gpui::test]
fn named_process_history_stacks_before_app_totals_at_narrow_width(cx: &mut TestAppContext) {
    // Reflow an installed named selection and compare every section boundary at 639 pixels.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.resize_process_history();
    harness.driver.choose_named_process();
    harness.driver.deliver_named_process_history();

    harness.driver.assert_named_process_narrow_order();

    harness.teardown();
}

macro_rules! process_refusal_case {
    ($name:ident, $kind:ident, $diagnostic:literal, $raw:literal) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Refuse only the named history through acceptance and inspect the installed App page.
            let mut harness = AppHoverHarness::setup(cx);
            harness.driver.resize_process_history();
            harness.driver.choose_named_process();
            harness.driver.deliver_named_process_history();

            harness
                .driver
                .deliver_process_refusal(ProcessRefusal::$kind);

            harness.driver.assert_process_refusal($diagnostic, $raw);
            harness.teardown();
        }
    };
}

process_refusal_case!(
    named_process_history_failure_keeps_app_totals_and_instances,
    Source,
    "Process group history · Unavailable: Captured group read refused",
    0
);
process_refusal_case!(
    named_process_history_projection_refusal_keeps_app_totals_and_instances,
    Projection,
    "Process group history · Unavailable: Invalid live chart quantity",
    0
);
process_refusal_case!(
    named_process_history_rejects_prevalidated_context_for_another_app,
    App,
    "Process group history · Unavailable: Process history belongs to another selected group",
    0
);
process_refusal_case!(
    named_process_history_rejects_prevalidated_context_for_another_name,
    Name,
    "Process group history · Unavailable: Process history belongs to another selected group",
    2
);

#[derive(Clone, Copy)]
enum ProcessRefusal {
    Source,
    Projection,
    App,
    Name,
}

#[gpui::test]
fn named_process_history_survives_current_instance_disappearance(cx: &mut TestAppContext) {
    // Remove the current group from detail while retaining its requested captured history.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.resize_process_history();
    harness.driver.choose_named_process();
    harness.driver.deliver_named_process_history();

    harness.driver.deliver_absent_named_process();

    assert_eq!(
        harness.driver.named_process_selection().as_deref(),
        Some("hex:61")
    );
    assert!(
        harness
            .driver
            .text_is_visible("chart-reading:live-process-group-cpu:240.0 %")
    );
    harness.teardown();
}

#[gpui::test]
fn all_process_groups_clears_the_named_history_and_instance_filter(cx: &mut TestAppContext) {
    // The All control clears both selection roles without substituting App totals as group charts.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.resize_process_history();
    harness.driver.choose_named_process();
    harness.driver.deliver_named_process_history();

    harness.driver.process_history_control("all-process-groups");
    harness.driver.deliver_all_processes();

    assert_eq!(harness.driver.named_process_selection(), None);
    assert!(
        !harness
            .driver
            .text_is_visible("live-process-group-cpu-plot")
    );
    assert!(harness.driver.text_is_visible("process-2"));
    assert!(harness.driver.text_is_visible("live-app-cpu-plot"));
    harness.teardown();
}

#[gpui::test]
fn named_process_history_preview_reuses_the_accepted_context(cx: &mut TestAppContext) {
    // Actual plot hover drives exact App details without requesting the group context again.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.resize_process_history();
    harness.driver.choose_named_process();
    harness.driver.deliver_named_process_history();
    let retained = harness.driver.named_process_context();

    harness.driver.hover_named_process_history();

    assert!(
        harness
            .driver
            .text_is_visible("chart-reading:live-process-group-cpu:120.0 %")
    );
    harness.driver.assert_named_preview_context(&retained);
    harness.driver.deliver_historical_details();
    assert!(harness.driver.text_is_visible("Historical worker"));
    assert!(
        harness
            .driver
            .text_is_visible("chart-reading:live-process-group-cpu:120.0 %")
    );
    harness.teardown();
}

impl AppHoverDriver {
    fn resize_process_history(&mut self) {
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(8000.0)));
        self.context.run_until_parked();
    }

    fn process_history_control(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).unwrap();
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }

    fn choose_named_process(&mut self) {
        self.process_history_control("executable-disk:hex:61:Unavailable *");
        assert_eq!(self.named_process_selection().as_deref(), Some("hex:61"));
    }

    fn deliver_named_process_history(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = AppEpoch::Accepted.snapshot(dashboard.generation);
            snapshot.process_history = Some(std::sync::Arc::new(ProcessHistory::prepare(
                payload(),
                &dashboard.request,
                &snapshot.selection,
            )));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn deliver_process_refusal(&mut self, refusal: ProcessRefusal) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = AppEpoch::Accepted.snapshot(dashboard.generation);
            let mut value = payload();
            if let ProcessRefusal::Projection = refusal {
                value["points"][0]["cpu_percent_one_core"] = json!(true);
            }
            let history = match refusal {
                ProcessRefusal::Source => {
                    ProcessHistory::Unavailable("Captured group read refused".into())
                }
                _ => ProcessHistory::prepare(value, &dashboard.request, &snapshot.selection),
            };
            match refusal {
                ProcessRefusal::App => {
                    dashboard.request.app.as_mut().unwrap().bytes_hex = "42".into();
                    snapshot.detail.as_mut().unwrap()["app"]["key"]["bytes_hex"] = json!("42");
                }
                ProcessRefusal::Name => dashboard.request.executable = Some("hex:62".into()),
                _ => {}
            }
            snapshot.process_history = Some(std::sync::Arc::new(history));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn assert_process_refusal(&mut self, diagnostic: &'static str, raw_index: usize) {
        assert!(self.text_is_visible(diagnostic));
        assert!(!self.text_is_visible("live-process-group-cpu-plot"));
        for selector in [
            "live-app-cpu-plot",
            "live-app-resident-plot",
            "live-app-footprint-plot",
            "live-app-power-plot",
            "live-app-disk-plot",
        ] {
            assert!(self.text_is_visible(selector), "{selector}");
        }
        assert!(self.text_is_visible("chart-reading:live-app-cpu:87.0 %"));
        assert!(
            self.text_is_visible("3 live of 3 members · membership complete · 9.0 J this interval")
        );
        assert!(self.text_is_visible(["process-0", "process-1", "process-2"][raw_index]));
        let expected = if raw_index == 2 { "hex:62" } else { "hex:61" };
        assert_eq!(self.named_process_selection().as_deref(), Some(expected));
    }

    fn deliver_absent_named_process(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let retained = dashboard.snapshot.as_ref().unwrap().process_history.clone();
            let mut snapshot = AppEpoch::Accepted.snapshot(dashboard.generation);
            snapshot.detail.as_mut().unwrap()["processes"] = json!([]);
            snapshot.process_history = retained;
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn deliver_all_processes(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.accept(AppEpoch::Accepted.snapshot(dashboard.generation));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn named_process_selection(&self) -> Option<String> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.request.executable.clone()
        })
    }

    fn assert_named_process_order(&mut self) {
        let group = self
            .context
            .debug_bounds("executable-disk:hex:61:Unavailable *")
            .unwrap();
        let history = self
            .context
            .debug_bounds("live-process-group-cpu-plot")
            .unwrap();
        let last = self
            .context
            .debug_bounds("live-process-group-disk-plot")
            .unwrap();
        let total = self.context.debug_bounds("live-app-cpu-plot").unwrap();
        let last_total = self.context.debug_bounds("live-app-disk-plot").unwrap();
        let raw = self.context.debug_bounds("process-0").unwrap();
        assert!(group.bottom() < history.top());
        assert!(last.bottom() < total.top());
        assert!(last_total.bottom() < raw.top());
    }

    fn assert_named_process_narrow_order(&mut self) {
        self.context
            .simulate_resize(gpui::size(gpui::px(639.0), gpui::px(8000.0)));
        self.context.run_until_parked();
        let cpu = self
            .context
            .debug_bounds("live-process-group-cpu-plot")
            .unwrap();
        let resident = self
            .context
            .debug_bounds("live-process-group-resident-plot")
            .unwrap();
        assert_eq!(cpu.left(), resident.left());
        assert!(cpu.bottom() < resident.top());
        self.assert_named_process_order();
    }

    fn named_process_context(&self) -> std::sync::Arc<ProcessHistory> {
        self.view.read_with(&self.context, |dashboard, _| {
            std::sync::Arc::clone(
                dashboard
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .process_history
                    .as_ref()
                    .unwrap(),
            )
        })
    }

    fn hover_named_process_history(&mut self) {
        let bounds = self
            .context
            .debug_bounds("live-process-group-cpu-plot")
            .unwrap();
        let plot = crate::charts::plot_bounds(bounds);
        self.context.simulate_mouse_move(
            gpui::point(
                plot.left() + plot.size.width * (898.5 / 900.0),
                plot.center().y,
            ),
            None,
            Modifiers::none(),
        );
        self.context.run_until_parked();
    }

    fn assert_named_preview_context(&self, retained: &std::sync::Arc<ProcessHistory>) {
        self.view.read_with(&self.context, |dashboard, _| {
            let preview = dashboard.preview.as_ref().unwrap();
            assert!(!preview.request.load_process_history);
            assert!(std::sync::Arc::ptr_eq(
                retained,
                preview.reading.process_history.as_ref().unwrap()
            ));
        });
    }
}
