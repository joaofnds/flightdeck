use super::*;
use crate::live_app_attribution_history::{AppAttributionHistory, Metric, tests::fixture};

#[gpui::test]
fn cpu_attribution_hover_updates_the_whole_page_and_reuses_its_cohort(cx: &mut TestAppContext) {
    // Drive the installed rate plot, retain its cohort while exact rankings load, then restore.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_attribution(Metric::Cpu);
    let anchors = harness.driver.attribution_anchors();

    harness
        .driver
        .hover_previous_second_on("live-cpu-apps-plot");

    assert!(harness.driver.reading_is_visible("130.00 % captured"));
    assert!(harness.driver.reading_is_visible("13.0 %"));
    assert!(
        harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0 %")
    );
    harness.driver.complete_attribution_preview();
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Historical:20.0 %")
    );
    harness.driver.assert_attribution_anchors(&anchors);

    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Sidebar);

    assert!(harness.driver.reading_is_visible("250.00 % captured"));
    assert!(harness.driver.reading_is_visible("87.0 %"));
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0 %")
    );
    harness.driver.assert_attribution_anchors(&anchors);
    harness.teardown();
}

#[gpui::test]
fn power_attribution_hover_updates_machine_power_and_exact_process_rankings(
    cx: &mut TestAppContext,
) {
    // The process-power history drives machine widgets at the same exact timestamp.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.seed_attribution(Metric::Power);

    harness
        .driver
        .hover_previous_second_on("live-power-apps-plot");

    assert!(harness.driver.reading_is_visible("1.50 W captured"));
    assert!(harness.driver.reading_is_visible("3.0 W"));
    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip-row:live-power-apps:0:App A:1.25 W")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    harness.driver.complete_attribution_preview();
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Process power:Historical:250.0 mW")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-power-apps:App A:1.25 W")
    );
    harness.teardown();
}

#[gpui::test]
fn attribution_failure_preserves_the_inspected_rate_and_reports_missing_rankings(
    cx: &mut TestAppContext,
) {
    // A failed exact response keeps captured history and never shows the committed App rank.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.seed_attribution(Metric::Power);
    harness
        .driver
        .hover_previous_second_on("live-power-apps-plot");

    harness.driver.fail_ranked_preview();

    assert!(harness.driver.reading_is_visible("1.50 W captured"));
    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: Ranking read failed")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Process power:Current:500.0 mW")
    );
    harness
        .driver
        .leave_plot("live-power-apps-plot", PlotExit::Sidebar);
    assert!(harness.driver.reading_is_visible("2.75 W captured"));
    harness.teardown();
}

#[gpui::test]
fn attribution_gaps_clear_readings_and_plot_exit_removes_the_tooltip(cx: &mut TestAppContext) {
    // Move the production hitbox from a captured interval into an unobserved second, then exit.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_attribution(Metric::Cpu);
    harness
        .driver
        .hover_previous_second_on("live-cpu-apps-plot");
    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip:live-cpu-apps")
    );
    let gap = harness
        .driver
        .second_position("live-cpu-apps-plot", 897.5 / 900.0);

    harness.driver.hover_position(gap);
    harness.driver.finish_missing_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-cpu-apps:App A:Unavailable")
    );
    assert!(!harness.driver.reading_is_visible("130.00 % captured"));
    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable at this observation")
    );
    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Sidebar);
    assert!(
        !harness
            .driver
            .reading_is_visible("chart-tooltip:live-cpu-apps")
    );
    assert!(harness.driver.reading_is_visible("250.00 % captured"));
    harness.teardown();
}

#[gpui::test]
fn cpu_attribution_and_ranking_share_a_wide_row_and_stack_narrow(cx: &mut TestAppContext) {
    // Read installed geometry across the same responsive breakpoint used by resident history.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_attribution(Metric::Cpu);
    harness.driver.assert_attribution_layout(Metric::Cpu, false);

    harness
        .driver
        .context
        .simulate_resize(gpui::size(px(620.0), px(5000.0)));
    harness.driver.context.run_until_parked();

    harness.driver.assert_attribution_layout(Metric::Cpu, true);
    harness.teardown();
}

#[gpui::test]
fn power_attribution_and_ranking_share_a_wide_row_and_stack_narrow(cx: &mut TestAppContext) {
    // Energy keeps its captured App chart beside exact ranking at wide widths.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.seed_attribution(Metric::Power);
    harness
        .driver
        .assert_attribution_layout(Metric::Power, false);

    harness
        .driver
        .context
        .simulate_resize(gpui::size(px(620.0), px(5000.0)));
    harness.driver.context.run_until_parked();

    harness
        .driver
        .assert_attribution_layout(Metric::Power, true);
    harness.teardown();
}

#[gpui::test]
fn attribution_legend_accepts_native_keydown_and_keyup_and_data_keeps_rates(
    cx: &mut TestAppContext,
) {
    // Exercise installed switches and Data controls rather than directly changing visibility.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.seed_attribution(Metric::Power);
    harness
        .driver
        .focus_resident_legend("chart-legend:live-power-apps:App A:2.50 W");

    harness.driver.press("space");
    harness.driver.release_legend_key("space");
    harness
        .driver
        .click_resident_legend("chart-data:live-power-apps");

    assert!(!harness.driver.reading_is_visible("2.50 W"));
    assert!(harness.driver.reading_is_visible("Available samples"));
    harness.driver.click_resident_legend("chart-data-close");
    harness
        .driver
        .focus_resident_legend("chart-legend:live-power-apps:App A:2.50 W");
    harness.driver.press("enter");
    harness.driver.release_legend_key("enter");
    harness
        .driver
        .click_resident_legend("chart-data:live-power-apps");
    assert!(harness.driver.reading_is_visible("2.50 W"));
    assert!(harness.driver.reading_is_visible("2 / 2"));
    harness.teardown();
}

#[gpui::test]
fn attribution_arrow_inspection_and_app_navigation_keep_the_exact_target(cx: &mut TestAppContext) {
    // Inspect through the installed plot's arrow key and activate its selected historical rank.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_attribution(Metric::Cpu);
    harness
        .driver
        .inspect_ranked_history_with_keyboard_on("live-cpu-apps-plot");
    harness.driver.complete_route_ranked_selection();

    harness
        .driver
        .click_ranked_app("ranking:CPU:Historical:20.0 %");

    harness.driver.assert_ranked_app_request(999, 1, "42");
    harness.teardown();
}

impl InputDriver {
    fn seed_attribution(&mut self, metric: Metric) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = attribution_snapshot(dashboard.generation);
            snapshot.rankings = Some(route_ranked_payload(&snapshot, RouteReading::Current));
            let history =
                AppAttributionHistory::prepare(metric, &fixture(metric), &snapshot.selection);
            assert!(history.panel().is_some(), "{}", history.coverage(None));
            snapshot.app_attribution_history = Some(Arc::new(history));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(3500.0)));
        self.context.run_until_parked();
    }

    fn complete_attribution_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick_at(Instant::now(), cx);
            let preview = dashboard.preview.as_ref().unwrap();
            assert!(!preview.request.load_app_attribution_history);
            let mut snapshot = attribution_snapshot(preview.generation.unwrap());
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = preview.target.identity.clone().unwrap();
            snapshot.selection["summary"] = snapshot.selection["trends"][0].clone();
            snapshot.rankings = Some(route_ranked_payload(&snapshot, RouteReading::Historical));
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn attribution_anchors(&self) -> (Arc<AppAttributionHistory>, std::rc::Rc<crate::data::Panel>) {
        self.view.read_with(&self.context, |dashboard, _| {
            let history = dashboard
                .snapshot
                .as_ref()
                .unwrap()
                .app_attribution_history
                .as_ref()
                .unwrap();
            let panel = dashboard
                .panels
                .iter()
                .find(|panel| Metric::from_panel(&panel.id).is_some())
                .unwrap();
            (Arc::clone(history), std::rc::Rc::clone(panel))
        })
    }

    fn assert_attribution_anchors(
        &self,
        retained: &(Arc<AppAttributionHistory>, std::rc::Rc<crate::data::Panel>),
    ) {
        let current = self.attribution_anchors();
        assert!(Arc::ptr_eq(&retained.0, &current.0));
        assert!(std::rc::Rc::ptr_eq(&retained.1, &current.1));
    }

    fn assert_attribution_layout(&mut self, metric: Metric, narrow: bool) {
        let (plot, ranking, card, wrapper) = match metric {
            Metric::Cpu => (
                "live-cpu-apps-plot",
                "ranking:CPU:Current:80.0 %",
                "chart-card:live-cpu-apps",
                "attribution-ranking:live-cpu-apps",
            ),
            Metric::Power => (
                "live-power-apps-plot",
                "ranking:Process power:Current:500.0 mW",
                "chart-card:live-power-apps",
                "attribution-ranking:live-power-apps",
            ),
        };
        let plot = self.chart_bounds(plot);
        let ranking = self.context.debug_bounds(ranking).unwrap();
        let card = self.context.debug_bounds(card).unwrap();
        let wrapper = self.context.debug_bounds(wrapper).unwrap();
        if narrow {
            assert!(plot.bottom() <= ranking.top());
            assert_eq!(wrapper.top() - card.bottom(), gpui::px(8.0));
        } else {
            assert!(plot.right() <= ranking.left());
            assert!(ranking.top() < plot.bottom());
            assert_eq!(wrapper.left() - card.right(), gpui::px(8.0));
        }
    }
}

fn attribution_snapshot(generation: u64) -> Box<Snapshot> {
    let mut snapshot = route_ranked_snapshot(generation);
    snapshot.selection["trends"][0]["cpu_percent"] = json!(13);
    snapshot.selection["trends"][1]["cpu_percent"] = json!(87);
    snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();

    snapshot
}
