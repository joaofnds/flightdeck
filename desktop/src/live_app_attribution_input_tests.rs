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

    assert!(harness.driver.reading_is_visible("130% captured"));
    assert!(harness.driver.reading_is_visible("13%"));
    assert!(
        harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0%")
    );
    harness.driver.complete_attribution_preview();
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Historical:20.0%")
    );
    harness.driver.assert_attribution_anchors(&anchors);

    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Sidebar);

    assert!(harness.driver.reading_is_visible("250% captured"));
    assert!(harness.driver.reading_is_visible("87%"));
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0%")
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
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Process power:2.8 W")
    );

    harness
        .driver
        .hover_previous_second_on("live-power-apps-plot");

    assert!(harness.driver.reading_is_visible("1.5 W captured"));
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Process power:1.5 W")
    );
    assert!(harness.driver.reading_is_visible("3 W"));
    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip-row:live-power-apps:1:App A:1.2 W")
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
            .reading_is_visible("summary-card:Process power:1.5 W")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Process power:Historical:250 mW")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-power-apps:App A:1.2 W")
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

    assert!(harness.driver.reading_is_visible("1.5 W captured"));
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Process power:1.5 W")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: Ranking read failed")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Process power:Current:500 mW")
    );
    harness
        .driver
        .leave_plot("live-power-apps-plot", PlotExit::Sidebar);
    assert!(harness.driver.reading_is_visible("2.8 W captured"));
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Process power:2.8 W")
    );
    harness.teardown();
}

#[gpui::test]
fn power_summary_withholds_a_reading_in_a_history_gap(cx: &mut TestAppContext) {
    // The installed App plot clears the summary when its selected second has no observation.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.seed_attribution(Metric::Power);
    let gap = harness
        .driver
        .second_position("live-power-apps-plot", 897.5 / 900.0);

    harness.driver.hover_position(gap);
    harness.driver.finish_missing_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Process power:Unavailable")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("summary-card:Process power:2.8 W")
    );
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
    assert!(!harness.driver.reading_is_visible("130% captured"));
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
    assert!(harness.driver.reading_is_visible("250% captured"));
    harness.teardown();
}

#[gpui::test]
fn a_pending_app_history_is_shown_as_being_read(cx: &mut TestAppContext) {
    // Goal: a newly opened page whose App history is still being read never calls it unavailable.
    // Method: accept a CPU page snapshot that leaves its App histories pending.
    let mut harness = InputHarness::setup_route(cx, "cpu");

    harness.driver.seed_pending_histories();

    assert!(harness.driver.reading_is_visible("Reading App history"));
    assert!(
        !harness
            .driver
            .reading_is_visible("App rate history unavailable at this observation")
    );
    harness.teardown();
}

#[gpui::test]
fn a_hover_while_the_page_reads_its_app_histories_leaves_that_read_running(
    cx: &mut TestAppContext,
) {
    // Goal: a hover asks the reader for nothing while the page reads its App histories, since a
    // hover's read would cancel the page's own.
    // Method: hover the CPU plot of a page whose App histories are pending, then tick the hover.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_pending_histories();
    harness.driver.hover_previous_second_on("live-cpu-plot");

    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 0);
    harness.teardown();
}

#[gpui::test]
fn the_pages_app_histories_arriving_during_a_hover_show_at_the_hovered_second(
    cx: &mut TestAppContext,
) {
    // Goal: a page that finishes reading its App histories during a hover shows them at once, at
    // the hovered second.
    // Method: hover the CPU plot while the App histories are pending, then deliver the page's
    // completed snapshot as the reader publishes it.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_pending_histories();
    harness.driver.hover_previous_second_on("live-cpu-plot");

    harness.driver.finish_pending_histories(Metric::Cpu);

    assert!(!harness.driver.reading_is_visible("Reading App history"));
    assert!(harness.driver.reading_is_visible("130% captured"));
    assert!(harness.driver.reading_is_visible("13%"));
    harness.teardown();
}

#[gpui::test]
fn app_histories_arriving_during_a_hover_leave_the_hovered_plot_in_place(cx: &mut TestAppContext) {
    // Goal: the App histories arriving during a hover do not move the hovered plot.
    // Method: hover the CPU temperature plot, below the App history card, while the App
    // histories are pending, then deliver them.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_pending_histories();
    harness
        .driver
        .hover_previous_second_on("live-cpu-temperature-plot");
    let hovered = harness.driver.chart_bounds("live-cpu-temperature-plot");

    harness.driver.finish_pending_histories(Metric::Cpu);

    let held = harness.driver.chart_bounds("live-cpu-temperature-plot");
    assert_eq!(held, hovered);
    harness.teardown();
}

#[gpui::test]
fn a_hover_that_outlives_the_pages_app_history_read_ends_on_leaving(cx: &mut TestAppContext) {
    // Goal: the App histories arriving during a hover do not keep the hover once the pointer
    // leaves the plot.
    // Method: hover the CPU temperature plot while the App histories are pending, deliver
    // them, then move the pointer to the sidebar.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_pending_histories();
    harness
        .driver
        .hover_previous_second_on("live-cpu-temperature-plot");
    harness.driver.finish_pending_histories(Metric::Cpu);

    harness
        .driver
        .leave_plot("live-cpu-temperature-plot", PlotExit::Sidebar);

    assert!(harness.driver.reading_is_visible("87%"));
    assert!(!harness.driver.reading_is_visible("13%"));
    harness.teardown();
}

#[gpui::test]
fn another_requests_reading_during_a_hover_on_a_loading_page_is_dropped(cx: &mut TestAppContext) {
    // Goal: while a hover waits for the page's App histories, only the page's own read can end
    // the wait.
    // Method: hover during the pending read, then deliver a completed snapshot of a later
    // request.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_pending_histories();
    harness.driver.hover_previous_second_on("live-cpu-plot");

    let received = harness.driver.deliver_later_requests_histories(Metric::Cpu);

    assert!(!received);
    assert!(harness.driver.reading_is_visible("Reading App history"));
    harness.teardown();
}

#[gpui::test]
fn a_failed_app_history_read_during_a_hover_is_reported_in_place_of_the_page(
    cx: &mut TestAppContext,
) {
    // Goal: a page whose App history read fails during a hover reports it as any failed read of
    // the page is reported, rather than leaving the hover waiting for that read.
    // Method: hover during the pending read, then deliver the page's read as failed.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_pending_histories();
    harness.driver.hover_previous_second_on("live-cpu-plot");

    harness.driver.fail_pending_histories();

    assert!(harness.driver.readings_are_hidden());
    assert_eq!(
        harness.driver.page_error().as_deref(),
        Some("App history read failed")
    );
    harness.teardown();
}

#[gpui::test]
fn a_hover_reads_what_the_pages_finished_app_histories_leave_out(cx: &mut TestAppContext) {
    // Goal: once the page has read its App histories, a hover reads the hovered second's App
    // totals the page keeps no window of.
    // Method: hover during the pending read, deliver the completed snapshot without ranked
    // seconds, then tick the hover.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_pending_histories();
    harness.driver.hover_previous_second_on("live-cpu-plot");
    harness.driver.finish_pending_histories(Metric::Cpu);

    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 1);
    assert!(
        harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    harness.teardown();
}

#[gpui::test]
fn a_step_reads_the_app_totals_the_pages_window_leaves_out(cx: &mut TestAppContext) {
    // Goal: a step to a second whose App totals the page keeps no window of reads them, once
    // the page has its reply.
    // Method: step left on a page whose App histories are read and whose window keeps no App
    // totals of the earlier second, then tick the step.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_attribution(Metric::Cpu);
    harness.driver.step_left_on("live-cpu-plot");

    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 1);
    harness.teardown();
}

#[gpui::test]
fn a_selected_step_leaves_the_worker_to_the_selections_read(cx: &mut TestAppContext) {
    // Goal: once Enter selects a stepped second, the step reads none of that second's App
    // totals itself, since a read of its own would cancel the selection's read, whose reply
    // brings them.
    // Method: step left on a page whose App histories are read and whose window keeps no App
    // totals of the earlier second, press Enter, then tick the step.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_attribution(Metric::Cpu);
    harness.driver.step_left_on("live-cpu-plot");
    harness.driver.press("enter");

    harness.driver.tick_hover();

    assert_eq!(harness.driver.requested_second(), Some(999));
    assert_eq!(harness.driver.hover_reads(), 0);
    harness.teardown();
}

#[gpui::test]
fn a_hover_during_a_selections_read_leaves_the_worker_to_that_read(cx: &mut TestAppContext) {
    // Goal: a pointer hover while the page reads a selected second reads nothing itself, since
    // a read of its own would cancel the selection's read.
    // Method: select 1001, which the page holds no readings of, hover a second whose App totals
    // the page keeps no window of, then tick the hover.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_attribution(Metric::Cpu);
    harness.driver.select_second(1001);
    harness.driver.hover_previous_second_on("live-cpu-plot");

    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 0);
    harness.teardown();
}

#[gpui::test]
fn the_reply_to_a_selection_reaches_the_page_under_a_hover(cx: &mut TestAppContext) {
    // Goal: the page takes the reply to its selection while the pointer hovers a chart, so
    // the hover never holds the page at the readings of the second it left.
    // Method: select 1001, hover a second whose App totals the page keeps no window of, tick
    // the hover, then hand the page the reply to its selection.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_attribution(Metric::Cpu);
    harness.driver.select_second(1001);
    harness.driver.hover_previous_second_on("live-cpu-plot");
    harness.driver.tick_hover();

    let taken = harness.driver.deliver_selection_reply(Metric::Cpu);

    assert!(taken);
    assert_eq!(harness.driver.accepted_second(), Some(1001));
    harness.teardown();
}

#[gpui::test]
fn a_step_on_a_page_that_follows_the_recording_reads_before_its_reply(cx: &mut TestAppContext) {
    // Goal: a step on a page that follows the newest second reads the App totals its window
    // leaves out while the page's own read is still out, since that read selects no second.
    // Method: go back to the newest second, step left before the page's reply, then tick the
    // step.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_attribution(Metric::Cpu);
    harness.driver.follow_latest();
    harness.driver.step_left_on("live-cpu-plot");

    harness.driver.tick_hover();

    assert_eq!(harness.driver.requested_second(), None);
    assert_eq!(harness.driver.hover_reads(), 1);
    harness.teardown();
}

#[gpui::test]
fn a_live_reading_arriving_during_a_hover_leaves_the_hovered_second_shown(cx: &mut TestAppContext) {
    // Goal: a hover holds the page at the hovered second while live readings arrive, once the
    // page has read its App histories.
    // Method: hover a page whose App histories are read, then deliver the page's next live
    // snapshot, which gives the hovered second another reading.
    let mut harness = InputHarness::setup_route(cx, "cpu");
    harness.driver.seed_attribution(Metric::Cpu);
    harness.driver.hover_previous_second_on("live-cpu-plot");

    let received = harness.driver.deliver_live_reading(Metric::Cpu);

    assert!(!received);
    assert!(harness.driver.reading_is_visible("13%"));
    assert!(!harness.driver.reading_is_visible("21%"));
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
fn gpu_attribution_and_ranking_share_a_wide_row_and_stack_narrow(cx: &mut TestAppContext) {
    // Goal: the GPU page lays GPU time by App out beside its ranking as the other App charts
    // are. Method: seed the GPU history and read the row's geometry wide and then narrow.
    let mut harness = InputHarness::setup_route(cx, "gpu");
    harness.driver.seed_attribution(Metric::Gpu);
    harness.driver.assert_attribution_layout(Metric::Gpu, false);

    harness
        .driver
        .context
        .simulate_resize(gpui::size(px(620.0), px(5000.0)));
    harness.driver.context.run_until_parked();

    harness.driver.assert_attribution_layout(Metric::Gpu, true);
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
        .focus_resident_legend("chart-legend:live-power-apps:App A:2.5 W");

    harness.driver.press("space");
    harness.driver.release_legend_key("space");
    harness
        .driver
        .click_resident_legend("chart-data:live-power-apps");

    assert!(!harness.driver.reading_is_visible("2.5 W"));
    assert!(harness.driver.reading_is_visible("Available samples"));
    harness.driver.click_resident_legend("chart-data-close");
    // Closing Data leaves the pointer where the button was, which may now lie over the plot.
    harness
        .driver
        .leave_plot("live-power-apps-plot", PlotExit::Sidebar);
    harness
        .driver
        .focus_resident_legend("chart-legend:live-power-apps:App A:2.5 W");
    harness.driver.press("enter");
    harness.driver.release_legend_key("enter");
    harness
        .driver
        .click_resident_legend("chart-data:live-power-apps");
    assert!(harness.driver.reading_is_visible("2.5 W"));
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
        .click_ranked_app("ranking:CPU:Historical:20.0%");

    harness.driver.assert_ranked_app_request(999, 1, "42");
    harness.teardown();
}

impl InputDriver {
    pub(crate) fn tick_hover(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
        });
        self.context.run_until_parked();
    }

    pub(crate) fn hover_reads(&self) -> u64 {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.preview_submissions)
    }

    fn seed_pending_histories(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = attribution_snapshot(dashboard.generation);
            snapshot.histories_pending = true;
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(3500.0)));
        self.context.run_until_parked();
    }

    pub(crate) fn seed_attribution(&mut self, metric: Metric) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = attributed_snapshot(dashboard.generation, metric);
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(3500.0)));
        self.context.run_until_parked();
    }

    fn finish_pending_histories(&mut self, metric: Metric) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = attributed_snapshot(dashboard.generation, metric);
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Delivers a later request's snapshot with its App histories, and returns whether the
    /// hover took it.
    fn deliver_later_requests_histories(&mut self, metric: Metric) -> bool {
        let received = self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = attributed_snapshot(dashboard.generation + 1, metric);
            let received = dashboard.preview_receive(Update::Ready(snapshot));
            cx.notify();
            received
        });
        self.context.run_until_parked();

        received
    }

    /// Hands the page the reader's answer to its selected second, and returns whether the page
    /// took it.
    fn deliver_selection_reply(&mut self, metric: Metric) -> bool {
        let received = self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = attributed_snapshot(dashboard.generation, metric);
            snapshot.selection["second"] = json!(dashboard.request.second);
            snapshot.selection["context_last_second"] = json!(dashboard.request.second);
            let received = dashboard.take_update(Update::Ready(snapshot));
            cx.notify();
            received
        });
        self.context.run_until_parked();

        received
    }

    fn fail_pending_histories(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            assert!(dashboard.preview_receive(Update::Failed {
                generation: dashboard.generation,
                message: "App history read failed".into(),
            }));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Delivers the page's next live snapshot, and returns whether the hover took it.
    fn deliver_live_reading(&mut self, metric: Metric) -> bool {
        let received = self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = attributed_snapshot(dashboard.generation, metric);
            snapshot.selection["trends"][0]["cpu_percent"] = json!(21);
            let received = dashboard.preview_receive(Update::Ready(snapshot));
            cx.notify();
            received
        });
        self.context.run_until_parked();

        received
    }

    fn complete_attribution_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
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
                "ranking:CPU:Current:80.0%",
                "chart-card:live-cpu-apps",
                "attribution-ranking:live-cpu-apps",
            ),
            Metric::Power => (
                "live-power-apps-plot",
                "ranking:Process power:Current:500 mW",
                "chart-card:live-power-apps",
                "attribution-ranking:live-power-apps",
            ),
            Metric::Disk => (
                "live-disk-apps-plot",
                "ranking:Disk I/O:Current:80 B/s",
                "chart-card:live-disk-apps",
                "attribution-ranking:live-disk-apps",
            ),
            Metric::Gpu => (
                "live-gpu-apps-plot",
                "ranking:GPU time:Current:25.0%",
                "chart-card:live-gpu-apps",
                "attribution-ranking:live-gpu-apps",
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

/// The page's snapshot once its App history of `metric` is read, with the current App totals.
fn attributed_snapshot(generation: u64, metric: Metric) -> Box<Snapshot> {
    let mut snapshot = attribution_snapshot(generation);
    snapshot.rankings = Some(route_ranked_payload(&snapshot, RouteReading::Current));
    let history = AppAttributionHistory::prepare(metric, &fixture(metric), &snapshot.selection);
    assert!(history.panel().is_some(), "{}", history.coverage(None));
    snapshot.app_attribution_history = Some(Arc::new(history));

    snapshot
}

fn attribution_snapshot(generation: u64) -> Box<Snapshot> {
    let mut snapshot = route_ranked_snapshot(generation);
    snapshot.selection["trends"][0]["cpu_percent"] = json!(13);
    snapshot.selection["trends"][1]["cpu_percent"] = json!(87);
    snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();

    snapshot
}
