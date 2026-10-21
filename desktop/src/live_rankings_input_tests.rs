use std::sync::Arc;

use gpui::{Modifiers, MouseButton, TestAppContext, px};
use serde_json::json;

use super::{InputDriver, InputHarness, PlotExit, accepted_snapshot};
use crate::live_hover::timings::tests::TimedHover;
use crate::live_source::tests::rankings::{kept_window_ranks, opened_window_ranks};
use crate::live_source::{Snapshot, Update};

#[path = "live_overview_app_history_input_tests.rs"]
mod overview_history;

#[gpui::test]
fn memory_hover_worker_failure_keeps_the_historical_scalar_without_current_app_links(
    cx: &mut TestAppContext,
) {
    // Fail the exact Memory worker generation, inspect its resident scalar, then leave.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_route_rankings();
    harness.driver.hover_previous_second_on("live-memory-plot");

    harness.driver.fail_ranked_preview();

    assert!(harness.driver.reading_is_visible("2 GiB app memory"));
    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: Ranking read failed")
    );
    assert!(!harness.driver.reading_is_visible("6 GiB app memory"));
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Resident memory:Current:1 GiB")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Resident memory:Historical:512 MiB")
    );

    harness
        .driver
        .leave_plot("live-memory-plot", PlotExit::Sidebar);

    assert!(harness.driver.reading_is_visible("6 GiB app memory"));
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:Current:1 GiB")
    );
    assert!(!harness.driver.reading_is_visible("2 GiB app memory"));
    harness.teardown();
}

#[gpui::test]
fn energy_hover_gap_clears_historical_readings_without_borrowing_the_current_app(
    cx: &mut TestAppContext,
) {
    // Move from exact historical Energy readings into a true gap, then return to committed data.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.seed_route_rankings();
    harness.driver.hover_previous_second_on("live-energy-plot");
    harness.driver.complete_route_ranked_preview();
    assert!(harness.driver.reading_is_visible("3 W"));
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Process power:Historical:250 mW")
    );
    let gap = harness
        .driver
        .second_position("live-energy-plot", 897.5 / 900.0);

    harness.driver.hover_position(gap);
    harness.driver.finish_missing_ranked_preview();

    let (second, reading) = harness.driver.chart_accessibility_reading("live-energy");
    assert_eq!(second, Some(998));
    assert!(reading.ends_with("Unavailable"));
    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable at this observation")
    );
    assert!(!harness.driver.reading_is_visible("9 W"));
    assert!(!harness.driver.reading_is_visible("3 W"));
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Process power:Current:500 mW")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Process power:Historical:250 mW")
    );

    harness
        .driver
        .leave_plot("live-energy-plot", PlotExit::Sidebar);

    assert!(harness.driver.reading_is_visible("9 W"));
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Process power:Current:500 mW")
    );
    harness.teardown();
}

#[gpui::test]
fn historical_memory_app_mouse_activation_preserves_identity_and_resets_offsets(
    cx: &mut TestAppContext,
) {
    // Select history through the installed chart keys, then click its resident App rank.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_route_rankings();
    harness
        .driver
        .inspect_ranked_history_with_keyboard_on("live-memory-plot");
    harness.driver.complete_route_ranked_selection();
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:Historical:512 MiB")
    );
    harness.driver.seed_stale_ranked_offsets();

    harness
        .driver
        .click_ranked_app("ranking:Resident memory:Historical:512 MiB");

    harness.driver.assert_ranked_app_request(999, 1, "42");
    harness.driver.assert_ranked_offsets_reset();
    harness.teardown();
}

#[gpui::test]
fn historical_energy_app_keyboard_activation_preserves_identity_and_resets_offsets(
    cx: &mut TestAppContext,
) {
    // Focus the historical power App row with the right mouse button, then activate with Enter.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.seed_route_rankings();
    harness
        .driver
        .inspect_ranked_history_with_keyboard_on("live-energy-plot");
    harness.driver.complete_route_ranked_selection();
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Process power:Historical:250 mW")
    );
    harness.driver.seed_stale_ranked_offsets();

    harness
        .driver
        .activate_ranked_app_with_keyboard("ranking:Process power:Historical:250 mW");

    harness.driver.assert_ranked_app_request(999, 1, "42");
    harness.driver.assert_ranked_offsets_reset();
    harness.teardown();
}

#[gpui::test]
fn memory_hover_loads_exact_app_rankings_and_restores_the_committed_reading(
    cx: &mut TestAppContext,
) {
    // Hover the installed Memory chart, deliver its exact response, then leave the plot.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_route_rankings();
    assert!(harness.driver.reading_is_visible("6 GiB app memory"));

    harness.driver.hover_previous_second_on("live-memory-plot");

    assert!(harness.driver.reading_is_visible("2 GiB app memory"));
    assert!(!harness.driver.reading_is_visible("6 GiB app memory"));
    assert!(
        harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Resident memory:Current:1 GiB")
    );

    harness.driver.complete_route_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:Historical:512 MiB")
    );
    assert!(harness.driver.reading_is_visible("2 GiB app memory"));
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Resident memory:Current:1 GiB")
    );
    assert_eq!(harness.driver.requested_publication(), Some(2));

    harness
        .driver
        .leave_plot("live-memory-plot", PlotExit::Sidebar);

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:Current:1 GiB")
    );
    assert!(harness.driver.reading_is_visible("6 GiB app memory"));
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Resident memory:Historical:512 MiB")
    );
    harness.teardown();
}

#[gpui::test]
fn energy_hover_loads_exact_app_rankings_and_restores_the_committed_reading(
    cx: &mut TestAppContext,
) {
    // Hover the installed Energy chart, deliver its exact response, then leave the plot.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.seed_route_rankings();
    assert!(harness.driver.reading_is_visible("9 W"));

    harness.driver.hover_previous_second_on("live-energy-plot");

    assert!(harness.driver.reading_is_visible("3 W"));
    assert!(!harness.driver.reading_is_visible("9 W"));
    assert!(
        harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Process power:Current:500 mW")
    );

    harness.driver.complete_route_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Process power:Historical:250 mW")
    );
    assert!(harness.driver.reading_is_visible("3 W"));
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Process power:Current:500 mW")
    );
    assert_eq!(harness.driver.requested_publication(), Some(2));

    harness
        .driver
        .leave_plot("live-energy-plot", PlotExit::Sidebar);

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Process power:Current:500 mW")
    );
    assert!(harness.driver.reading_is_visible("9 W"));
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Process power:Historical:250 mW")
    );
    harness.teardown();
}

#[gpui::test]
fn energy_rankings_show_the_selected_app_power(cx: &mut TestAppContext) {
    // Read the installed Energy card using the existing exact selected-App ranking fixture.
    let mut harness = InputHarness::setup_route(cx, "energy");

    harness.driver.seed_ranked_overview();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Process power:Current:500 mW")
    );
    harness.teardown();
}

#[gpui::test]
fn cpu_rankings_show_the_selected_app_cpu(cx: &mut TestAppContext) {
    // Read the installed CPU card without replacing its production row composition.
    let mut harness = InputHarness::setup_route(cx, "cpu");

    harness.driver.seed_ranked_overview();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0%")
    );
    harness.teardown();
}

#[gpui::test]
fn memory_rankings_show_the_selected_app_resident_memory(cx: &mut TestAppContext) {
    // Read the resident gauge card on Memory, separate from the machine memory summary.
    let mut harness = InputHarness::setup_route(cx, "memory");

    harness.driver.seed_ranked_overview();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:Current:1 GiB")
    );
    harness.teardown();
}

#[gpui::test]
fn overview_rankings_show_current_measurements_and_explicit_source_limits(cx: &mut TestAppContext) {
    // Read real production rank nodes after providing one exact shared-response page.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0%")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:Current:1 GiB")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Process power:Current:500 mW")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("ranking-uncaptured:Network")
    );
    // The fixture's selection has no GPU time the app sampled, and the GPU card says so.
    assert!(
        harness
            .driver
            .reading_is_visible("GPU time by App is sampled only while the app runs")
    );
    harness.teardown();
}

#[gpui::test]
fn ranking_cells_compare_unequal_amounts_with_proportional_fills(cx: &mut TestAppContext) {
    // Measure production GPUI tracks and fills for exact CPU amounts in a four-to-one ratio.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.set_ranked_cpu_amounts(80, 20);

    harness
        .driver
        .assert_ranked_fill("ranking-fill:CPU:41", "ranking-track:CPU:41", 1.0);
    harness
        .driver
        .assert_ranked_fill("ranking-fill:CPU:42", "ranking-track:CPU:42", 0.25);
    harness.teardown();
}

#[gpui::test]
fn ranking_cells_keep_measured_zero_fills_empty(cx: &mut TestAppContext) {
    // An all-zero population has measured values without a fabricated positive comparison bar.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.set_ranked_cpu_amounts(0, 0);

    harness
        .driver
        .assert_ranked_fill("ranking-fill:CPU:41", "ranking-track:CPU:41", 0.0);
    harness
        .driver
        .assert_ranked_fill("ranking-fill:CPU:42", "ranking-track:CPU:42", 0.0);
    harness.teardown();
}

#[gpui::test]
fn ranked_metric_help_explains_the_measured_quantity(cx: &mut TestAppContext) {
    // Hover the installed About control and advance GPUI's virtual tooltip clock.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();

    harness.driver.hover_ranked_help();

    assert!(harness.driver.reading_is_visible(
        "One-core CPU time divided by the captured continuous interval. Known subtotals \
         exclude unavailable members; zero is a measured reading."
    ));
    harness.teardown();
}

#[gpui::test]
fn a_ranking_only_failure_preserves_the_exact_historical_chart(cx: &mut TestAppContext) {
    // A successful historical snapshot can refuse App ranks while its scalar chart stays usable.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.hover_previous_second_on("live-cpu-plot");

    harness.driver.complete_unavailable_ranked_preview();

    assert!(harness.driver.reading_is_visible("13%"));
    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: App response bound")
    );
    assert!(!harness.driver.reading_is_visible("87%"));
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0%")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Historical:20.0%")
    );
    harness.teardown();
}

#[gpui::test]
fn historical_hover_hides_current_ranks_until_its_exact_payload_arrives(cx: &mut TestAppContext) {
    // Keep charts resident while the App payload loads, then restore current ranks on plot exit.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();

    harness.driver.hover_previous_second_on("live-cpu-plot");

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
    harness.driver.complete_ranked_preview();
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Historical:20.0%")
    );
    assert_eq!(harness.driver.requested_publication(), Some(2));
    harness
        .driver
        .leave_plot("live-cpu-plot", PlotExit::Sidebar);
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0%")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Historical:20.0%")
    );
    harness.teardown();
}

#[gpui::test]
fn historical_hover_shows_the_kept_ranks_of_its_second_in_the_frame_of_the_move(
    cx: &mut TestAppContext,
) {
    // Goal: a hover on a second whose ranks the page keeps shows them as the pointer moves, with
    // no loading notice.
    // Method: keep a sealed recording's window ranks beside a committed Overview, and hover the
    // earlier second, publication one of the same run and boot, whose largest App, App00600,
    // held 600 KiB of resident memory.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.keep_window_ranks(kept_window_ranks());

    harness.driver.hover_previous_second_on("live-cpu-plot");

    assert!(
        !harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:App00600:0.59 MiB")
    );
    harness.teardown();
}

#[gpui::test]
fn an_arrow_step_to_a_second_whose_ranks_are_kept_shows_them_and_reads_nothing(
    cx: &mut TestAppContext,
) {
    // Goal: an arrow step to a second whose ranks the page keeps shows them in the frame that
    // handles the key, with no loading notice, and leaves the worker to the page's own reads.
    // Method: keep a sealed recording's window ranks beside a committed Overview, step left on
    // the CPU chart to the earlier second, publication one of the same run and boot, whose
    // largest App, App00600, held 600 KiB of resident memory, and tick the step.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.keep_window_ranks(kept_window_ranks());

    harness.driver.step_left_on("live-cpu-plot");

    assert!(
        !harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:App00600:0.59 MiB")
    );
    harness.driver.tick_hover();
    assert_eq!(harness.driver.hover_reads(), 0);
    harness.teardown();
}

#[gpui::test]
fn a_hover_that_waits_for_the_pages_app_histories_is_timed_once_they_complete_it(
    cx: &mut TestAppContext,
) {
    // Goal: a hover that waits for the page's App histories keeps its timing while it waits,
    // and ends it once the ranks they bring complete its readings.
    // Method: hover the earlier second of an Overview whose App histories are pending, then
    // deliver the page's read with the window's kept ranks.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview_reading_histories();
    harness.driver.hover_previous_second_on("live-cpu-plot");

    harness
        .driver
        .finish_histories_with_ranks(kept_window_ranks());

    assert_eq!(
        harness.driver.timed_hovers(),
        [TimedHover::complete("overview", 999)]
    );
    harness.teardown();
}

#[gpui::test]
fn historical_hover_on_a_second_whose_ranks_are_kept_reads_nothing(cx: &mut TestAppContext) {
    // Goal: a hover the kept ranks complete leaves the worker to the page's own reads.
    // Method: keep a sealed recording's window ranks beside a committed Overview, hover the
    // earlier second, publication one of the same run and boot, and tick the hover.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.keep_window_ranks(kept_window_ranks());

    harness.driver.hover_previous_second_on("live-cpu-plot");
    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 0);
    harness.teardown();
}

#[gpui::test]
fn a_first_hover_on_a_newly_opened_page_shows_the_ranks_its_saved_minutes_keep(
    cx: &mut TestAppContext,
) {
    // Goal: the first hover on a newly opened page shows a saved second's top Apps as the pointer
    // moves, from the ranks the window's sealed minutes keep, with no loading notice.
    // Method: open an Overview whose App histories are pending with the ranks the first read of
    // a recording sealed with its App ranks brings, and hover the earlier second, publication
    // one of the same run and boot, whose largest App, App00600, held 600 KiB of resident memory.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview_reading_histories();
    harness.driver.keep_window_ranks(opened_window_ranks());

    harness.driver.hover_previous_second_on("live-cpu-plot");

    assert!(
        !harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:App00600:0.59 MiB")
    );
    harness.teardown();
}

#[gpui::test]
fn failed_historical_rankings_cannot_restore_current_app_links(cx: &mut TestAppContext) {
    // Deliver a worker error for the exact hover generation and inspect the composed failure.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.hover_previous_second_on("live-cpu-plot");

    harness.driver.fail_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: Ranking read failed")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0%")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Historical:20.0%")
    );
    harness.teardown();
}

#[gpui::test]
fn a_missing_historical_observation_has_no_ranked_app_links(cx: &mut TestAppContext) {
    // A true gap has no identity and cannot borrow App totals from the committed selection.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    let position = harness
        .driver
        .second_position("live-cpu-plot", 897.5 / 900.0);

    harness.driver.hover_position(position);
    harness.driver.finish_missing_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable at this observation")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0%")
    );
    harness.teardown();
}

#[gpui::test]
fn a_paused_same_second_rank_link_keeps_the_earlier_observation(cx: &mut TestAppContext) {
    // A later resident sibling must not replace the exact identity captured by the row.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.add_later_ranked_sibling();

    harness.driver.click_ranked_app("ranking:CPU:Current:80.0%");

    harness.driver.assert_ranked_app_request(1000, 2, "41");
    harness.teardown();
}

#[gpui::test]
fn a_keyboard_inspection_rank_link_commits_its_displayed_historical_identity(
    cx: &mut TestAppContext,
) {
    // Dispatch the chart's actual arrow key before opening its selected historical App detail.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.inspect_ranked_history_with_keyboard();
    harness.driver.complete_ranked_selection();

    harness
        .driver
        .click_ranked_app("ranking:CPU:Historical:20.0%");

    harness.driver.assert_ranked_app_request(999, 1, "42");
    harness.teardown();
}

impl InputDriver {
    fn set_ranked_cpu_amounts(&mut self, first: u64, second: u64) {
        use num_traits::ToPrimitive;
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = dashboard.snapshot.as_mut().unwrap();
            let rankings = Arc::get_mut(snapshot.rankings.as_mut().unwrap()).unwrap();
            let crate::live_rankings::Rankings::Ready(rankings) = rankings else {
                panic!()
            };
            let entries = &mut rankings.metrics[0].entries;
            entries[0].amount = first;
            entries[0].rate = Some(first.to_f64().unwrap());
            entries.push(crate::live_rankings::Entry {
                app: Arc::new(crate::live_rankings::App {
                    key: crate::live_source::AppKey {
                        kind: "bundle".into(),
                        bytes_hex: "42".into(),
                    },
                    name: "Lower".into(),
                }),
                amount: second,
                rate: Some(second.to_f64().unwrap()),
                measured_members: 1,
                unavailable_members: 0,
            });
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn assert_ranked_fill(&mut self, fill: &'static str, track: &'static str, fraction: f32) {
        let fill = self.context.debug_bounds(fill).expect(fill);
        let track = self.context.debug_bounds(track).expect(track);
        assert!((f32::from(fill.size.width - track.size.width * fraction)).abs() < 0.5);
    }

    fn hover_ranked_help(&mut self) {
        let bounds = self.context.debug_bounds("ranking-help:CPU").unwrap();
        self.context
            .simulate_mouse_move(bounds.center(), None, Modifiers::none());
        self.context.run_until_parked();
        self.context
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        self.context.run_until_parked();
    }

    fn seed_ranked_overview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.accept(ranked_overview(dashboard.generation));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(3500.0)));
        self.context.run_until_parked();
    }

    /// Seeds the Overview's first readings, which leave its App histories, and the window's
    /// ranks with them, to the page's next read.
    fn seed_ranked_overview_reading_histories(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = ranked_overview(dashboard.generation);
            snapshot.histories_pending = true;
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(3500.0)));
        self.context.run_until_parked();
    }

    /// Delivers the page's read of its App histories, which brings the window's kept ranks.
    fn finish_histories_with_ranks(&mut self, ranks: Arc<viewer::WindowRanks>) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = ranked_overview(dashboard.generation);
            snapshot.app_ranks = Some(ranks);
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn timed_hovers(&self) -> Vec<TimedHover> {
        self.view.read_with(&self.context, |dashboard, _| {
            TimedHover::latest(&dashboard.hover_timings)
        })
    }

    fn keep_window_ranks(&mut self, ranks: Arc<viewer::WindowRanks>) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.snapshot.as_mut().unwrap().app_ranks = Some(ranks);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    pub(super) fn complete_ranked_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
            let preview = dashboard.preview.as_ref().unwrap();
            let mut snapshot = accepted_snapshot(preview.generation.unwrap());
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = preview.target.identity.clone().unwrap();
            snapshot.rankings = Some(ranked_payload(&snapshot, "Historical", "42", 20));
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    pub(super) fn complete_unavailable_ranked_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
            let preview = dashboard.preview.as_ref().unwrap();
            let mut snapshot = accepted_snapshot(preview.generation.unwrap());
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = preview.target.identity.clone().unwrap();
            snapshot.selection["summary"] = snapshot.selection["trends"][0].clone();
            snapshot.selection["summary"]["cpu_percent"] = json!(13);
            snapshot.rankings = Some(Arc::new(crate::live_rankings::Rankings::Unavailable(
                "App response bound".into(),
            )));
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    pub(super) fn fail_ranked_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
            assert!(dashboard.preview_receive(Update::Failed {
                generation: dashboard.preview.as_ref().unwrap().generation.unwrap(),
                message: "Ranking read failed".into(),
            }));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    pub(super) fn finish_missing_ranked_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
        });
        self.context.run_until_parked();
    }

    fn add_later_ranked_sibling(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = dashboard.snapshot.as_mut().unwrap();
            let mut sibling = snapshot.selection["trends"][1].clone();
            sibling["publication"] = json!(3);
            snapshot.selection["trends"]
                .as_array_mut()
                .unwrap()
                .push(sibling);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn inspect_ranked_history_with_keyboard(&mut self) {
        self.inspect_ranked_history_with_keyboard_on("live-cpu-plot");
    }

    pub(super) fn inspect_ranked_history_with_keyboard_on(&mut self, selector: &'static str) {
        self.step_left_on(selector);
        self.press("enter");
        assert_eq!(self.requested_second(), Some(999));
        assert_eq!(self.requested_publication(), Some(1));
    }

    /// Focuses the chart with a right click, which selects nothing, and steps it left.
    fn step_left_on(&mut self, selector: &'static str) {
        let bounds = self.chart_bounds(selector);
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
        self.press("left");
    }

    pub(super) fn complete_ranked_selection(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = dashboard.request.identity.clone().unwrap();
            snapshot.selection["trends"]
                .as_array_mut()
                .unwrap()
                .truncate(1);
            snapshot.selection["summary"] = snapshot.selection["trends"][0].clone();
            snapshot.rankings = Some(ranked_payload(&snapshot, "Historical", "42", 20));
            assert!(dashboard.take_update(crate::live_source::Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    pub(super) fn click_ranked_app(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).expect(selector);
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }

    pub(super) fn assert_ranked_app_request(&self, second: u64, publication: u64, key: &str) {
        let mut identity = accepted_snapshot(0).selection["selected"].clone();
        identity["publication"] = json!(publication);
        let expected = crate::live_hover::Target::exact(second, &identity).unwrap();
        self.view.read_with(&self.context, |dashboard, _| {
            assert_eq!(dashboard.request.route, "apps");
            assert_eq!(dashboard.request.second, Some(expected.second));
            assert_eq!(dashboard.request.identity, expected.identity);
            assert_eq!(
                dashboard.request.app,
                Some(crate::live_source::AppKey {
                    kind: "bundle".into(),
                    bytes_hex: key.into(),
                })
            );
            assert!(dashboard.preview.is_none());
        });
    }
}

/// The Overview's readings of its committed second, ranked, at 87% CPU after 13%.
fn ranked_overview(generation: u64) -> Box<Snapshot> {
    let mut snapshot = accepted_snapshot(generation);
    snapshot.selection["trends"][0]["cpu_percent"] = json!(13);
    snapshot.selection["trends"][1]["cpu_percent"] = json!(87);
    snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
    snapshot.rankings = Some(ranked_payload(&snapshot, "Current", "41", 80));

    snapshot
}

fn ranked_payload(
    snapshot: &Snapshot,
    name: &str,
    key: &str,
    cpu: u32,
) -> Arc<crate::live_rankings::Rankings> {
    let cpu_rate = Some(f64::from(cpu));
    Arc::new(crate::live_rankings::fixture::one_bundle(
        &snapshot.selection["selected"],
        name,
        key,
        [
            (u64::from(cpu), cpu_rate),
            (1_073_741_824, None),
            (u64::from(cpu), Some(0.5)),
            (u64::from(cpu), cpu_rate),
        ],
    ))
}

impl InputDriver {
    fn seed_route_rankings(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = route_ranked_snapshot(dashboard.generation);
            snapshot.rankings = Some(route_ranked_payload(&snapshot, RouteReading::Current));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(3500.0)));
        self.context.run_until_parked();
    }

    fn complete_route_ranked_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
            let preview = dashboard.preview.as_ref().unwrap();
            let mut snapshot = route_ranked_snapshot(preview.generation.unwrap());
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = preview.target.identity.clone().unwrap();
            snapshot.selection["summary"] = snapshot.selection["trends"][0].clone();
            snapshot.selection["trends"]
                .as_array_mut()
                .unwrap()
                .truncate(1);
            assert_eq!(
                snapshot.selection["summary"],
                preview.reading.selection["summary"]
            );
            snapshot.rankings = Some(route_ranked_payload(&snapshot, RouteReading::Historical));
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }
}

fn route_ranked_snapshot(generation: u64) -> Box<Snapshot> {
    let mut snapshot = accepted_snapshot(generation);
    snapshot.selection["trends"][0]["memory_active_bytes"] = json!(2_147_483_648_u64);
    snapshot.selection["trends"][0]["memory_anonymous_bytes"] = json!(2_147_483_648_u64);
    snapshot.selection["trends"][0]["memory_purgeable_bytes"] = json!(0);
    snapshot.selection["trends"][0]["power_watts"] = json!(3.0);
    snapshot.selection["trends"][1]["memory_active_bytes"] = json!(6_442_450_944_u64);
    snapshot.selection["trends"][1]["memory_anonymous_bytes"] = json!(6_442_450_944_u64);
    snapshot.selection["trends"][1]["memory_purgeable_bytes"] = json!(0);
    snapshot.selection["trends"][1]["power_watts"] = json!(9.0);
    snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();

    snapshot
}

#[derive(Clone, Copy)]
enum RouteReading {
    Current,
    Historical,
}

fn route_ranked_payload(
    snapshot: &Snapshot,
    reading: RouteReading,
) -> Arc<crate::live_rankings::Rankings> {
    let (name, key, cpu, resident_bytes, energy_nj, power_watts) = match reading {
        RouteReading::Current => ("Current", "41", 80, 1_073_741_824, 500_000_000, 0.5),
        RouteReading::Historical => ("Historical", "42", 20, 536_870_912, 250_000_000, 0.25),
    };
    let mut result = ranked_payload(snapshot, name, key, cpu);
    let crate::live_rankings::Rankings::Ready(rankings) = Arc::get_mut(&mut result).unwrap() else {
        panic!("Route fixture requires measured App rankings");
    };
    rankings.metrics[1].entries[0].amount = resident_bytes;
    rankings.metrics[2].entries[0].amount = energy_nj;
    rankings.metrics[2].entries[0].rate = Some(power_watts);

    result
}

impl InputDriver {
    fn complete_route_ranked_selection(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = route_ranked_snapshot(dashboard.generation);
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = dashboard.request.identity.clone().unwrap();
            snapshot.selection["summary"] = snapshot.selection["trends"][0].clone();
            snapshot.selection["trends"]
                .as_array_mut()
                .unwrap()
                .truncate(1);
            snapshot.rankings = Some(route_ranked_payload(&snapshot, RouteReading::Historical));
            assert!(dashboard.take_update(crate::live_source::Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn seed_stale_ranked_offsets(&mut self) {
        self.view.update(&mut self.context, |dashboard, _| {
            dashboard.request.offset = 512;
            dashboard.request.apps_offset = 512;
            dashboard.request.members_offset = 512;
        });
    }

    fn assert_ranked_offsets_reset(&self) {
        self.view.read_with(&self.context, |dashboard, _| {
            assert_eq!(dashboard.request.offset, 0);
            assert_eq!(dashboard.request.apps_offset, 0);
            assert_eq!(dashboard.request.members_offset, 0);
        });
    }

    fn activate_ranked_app_with_keyboard(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).expect(selector);
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context.run_until_parked();
        assert!(self.reading_is_visible(selector));
        self.press("enter");
        self.context.run_until_parked();
    }
}

#[gpui::test]
fn memory_shows_interval_resident_leaders_separately_from_current_top_apps(
    cx: &mut TestAppContext,
) {
    // Display an interval cohort whose excluded ninth App leads the current sample.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_interval_resident_leaders();
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:App I:0.000095 MiB")
    );

    assert!(
        harness
            .driver
            .reading_is_visible("live-memory-app-resident-plot")
    );

    assert!(harness.driver.reading_is_visible("Other"));
    assert!(harness.driver.reading_is_visible("App A"));
    harness.teardown();
}

#[gpui::test]
fn resident_hover_sorts_adjacent_exact_byte_counts_above_plot_precision(cx: &mut TestAppContext) {
    // Adjacent bytes share an f32 plot value but must retain their exact readout ordering.
    let mut harness = InputHarness::setup_route(cx, "memory");
    let mut history = interval_resident_history();
    history["points"][1]["apps"][0] = resident_history_measure(16_777_217);
    history["points"][1]["other"]["resident_bytes"] = resident_history_measure(16_777_216);
    harness.driver.seed_resident_leaders(history);

    harness
        .driver
        .hover_selected_second_on("live-memory-app-resident-plot");

    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip-row:live-memory-app-resident:0:App A:16 MiB")
    );
    harness.teardown();
}

impl InputDriver {
    fn seed_interval_resident_leaders(&mut self) {
        self.seed_resident_leaders(interval_resident_history());
    }

    fn seed_resident_leaders(&mut self, history: serde_json::Value) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = route_ranked_snapshot(dashboard.generation);
            let mut rankings = ranked_payload(&snapshot, "App I", "4170702049", 80);
            let crate::live_rankings::Rankings::Ready(ready) = Arc::get_mut(&mut rankings).unwrap()
            else {
                panic!("Resident fixture requires captured rankings");
            };
            let app = Arc::new(crate::live_rankings::App {
                name: "App I".into(),
                key: crate::live_source::AppKey {
                    kind: "executable".into(),
                    bytes_hex: "4170702049".into(),
                },
            });
            for metric in &mut ready.metrics {
                metric.entries[0].app = Arc::clone(&app);
            }
            ready.metrics[1].entries[0].amount = 100;
            snapshot.rankings = Some(rankings);
            snapshot.history = Some(history);
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(3500.0)));
        self.context.run_until_parked();
    }
}

fn interval_resident_history() -> serde_json::Value {
    let cohort: Vec<_> = (b'A'..=b'H')
        .map(|name| {
            json!({
        "key":{"kind":"executable", "bytes_hex":format!("41707020{name:02x}"),
            "display":format!("App {}", char::from(name))},
        "sum_bytes":"200", "samples":2})
        })
        .collect();
    let earlier: Vec<_> = (0..8).map(|_| resident_history_measure(199)).collect();
    let current: Vec<_> = (0..8).map(|_| resident_history_measure(1)).collect();
    json!({"selected":resident_history_observation(2), "context_seconds":900,
        "cohort":cohort,"points":[
            {"observation":resident_history_observation(1), "segment":"0",
                "source_available":true,"membership_complete":true,
                "ungrouped_members":0,"usage_refused":0,"usage_gone":0,"apps":earlier,
                "other":{"app_count":1,"resident_bytes":resident_history_measure(0)}},
            {"observation":resident_history_observation(2), "segment":"0",
                "source_available":true,"membership_complete":true,
                "ungrouped_members":0,"usage_refused":0,"usage_gone":0,"apps":current,
                "other":{"app_count":1,"resident_bytes":resident_history_measure(100)}}],
        "coverage":{"retention_limited":false,"live_retention_limited":false,
            "observations_retained":2,"committed_observations":0,"context_app_count":9,
            "context_app_count_max":24000,"context_key_bytes":45,
            "context_key_bytes_max":8_388_608,"cache_bytes":0,"cache_bytes_max":67_108_864}})
}

fn resident_history_observation(publication: u64) -> serde_json::Value {
    let second = publication + 998;
    json!({"recording":["5","6"],"stamp":{"boot":["3","4"],"run":["1","2"],
        "publication":publication.to_string(),"continuous_ns":(second*1_000_000_000).to_string(),
        "awake_ns":(second*1_000_000_000).to_string()},"run_tick":publication.to_string(),
        "wall_ns":(second*1_000_000_000).to_string()})
}

fn resident_history_measure(known: u64) -> serde_json::Value {
    json!({"known":known.to_string(),"measured_members":1,
        "unavailable_members":0,"overflowed":false})
}

#[gpui::test]
fn resident_history_hover_keeps_interval_cohort_and_exact_bytes_through_loading_and_restore(
    cx: &mut TestAppContext,
) {
    // Inspect the installed resident plot while exact historical App rankings load beside it.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_interval_resident_leaders();
    let retained = harness.driver.resident_history_anchors();
    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-memory-app-resident:App A:0.000001 MiB")
    );

    harness
        .driver
        .hover_previous_second_on("live-memory-app-resident-plot");

    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-memory-app-resident:App A:0.00019 MiB")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    harness.driver.complete_route_ranked_preview();
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:Historical:512 MiB")
    );
    harness.driver.assert_resident_history_anchors(&retained);

    harness
        .driver
        .leave_plot("live-memory-app-resident-plot", PlotExit::Sidebar);

    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-memory-app-resident:App A:0.000001 MiB")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:App I:0.000095 MiB")
    );
    harness.driver.assert_resident_history_anchors(&retained);
    harness.teardown();
}

#[gpui::test]
fn resident_history_worker_failure_retains_the_exact_historical_readout(cx: &mut TestAppContext) {
    // Fail the preview worker after inspecting an accepted resident context, then leave the plot.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_interval_resident_leaders();
    harness
        .driver
        .hover_previous_second_on("live-memory-app-resident-plot");

    harness.driver.fail_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-memory-app-resident:App A:0.00019 MiB")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: Ranking read failed")
    );
    harness
        .driver
        .leave_plot("live-memory-app-resident-plot", PlotExit::Sidebar);
    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-memory-app-resident:App A:0.000001 MiB")
    );
    harness.teardown();
}

#[gpui::test]
fn resident_history_missing_second_shows_no_borrowed_residents(cx: &mut TestAppContext) {
    // Move from a measured historical second to a true context gap through the real hitbox.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_interval_resident_leaders();
    harness
        .driver
        .hover_previous_second_on("live-memory-app-resident-plot");
    let gap = harness
        .driver
        .second_position("live-memory-app-resident-plot", 897.5 / 900.0);

    harness.driver.hover_position(gap);
    harness.driver.finish_missing_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-memory-app-resident:App A:Unavailable")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("chart-legend:live-memory-app-resident:App A:0.00019 MiB")
    );
    harness.teardown();
}

#[gpui::test]
fn resident_history_and_rankings_share_a_wide_row_and_stack_in_a_narrow_view(
    cx: &mut TestAppContext,
) {
    // Compare installed plot and ranking geometry at desktop and narrow window widths.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_interval_resident_leaders();
    harness.driver.assert_resident_attribution_layout(false);

    harness
        .driver
        .context
        .simulate_resize(gpui::size(px(620.0), px(5000.0)));
    harness.driver.context.run_until_parked();

    harness.driver.assert_resident_attribution_layout(true);
    harness.teardown();
}

impl InputDriver {
    fn resident_history_anchors(
        &self,
    ) -> (
        Arc<crate::live_resident_history::ResidentHistory>,
        std::rc::Rc<crate::data::Panel>,
    ) {
        self.view.read_with(&self.context, |dashboard, _| {
            let history = dashboard
                .snapshot
                .as_ref()
                .unwrap()
                .resident_history
                .as_ref()
                .unwrap();
            let panel = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == crate::live_resident_history::PANEL_ID)
                .unwrap();
            (Arc::clone(history), std::rc::Rc::clone(panel))
        })
    }

    fn assert_resident_history_anchors(
        &self,
        retained: &(
            Arc<crate::live_resident_history::ResidentHistory>,
            std::rc::Rc<crate::data::Panel>,
        ),
    ) {
        let accepted = self.resident_history_anchors();
        assert!(Arc::ptr_eq(&retained.0, &accepted.0));
        assert!(std::rc::Rc::ptr_eq(&retained.1, &accepted.1));
    }

    fn assert_resident_attribution_layout(&mut self, narrow: bool) {
        let plot = self.chart_bounds("live-memory-app-resident-plot");
        let rank = self
            .context
            .debug_bounds("ranking:Resident memory:App I:0.000095 MiB")
            .unwrap();
        if narrow {
            assert!(plot.bottom() <= rank.top());
        } else {
            assert!(plot.right() <= rank.left());
            assert!(rank.top() < plot.bottom());
        }
    }
}

#[gpui::test]
fn resident_history_same_basename_apps_have_independent_legend_toggles(cx: &mut TestAppContext) {
    // Address two captured bundle paths sharing a basename and toggle each through the UI.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness
        .driver
        .seed_resident_leaders(same_basename_resident_history());
    assert!(harness.driver.reading_is_visible("/A/Foo"));
    assert!(harness.driver.reading_is_visible("/B/Foo"));
    assert!(!harness.driver.resident_legend_is_hidden("/A/Foo"));
    assert!(!harness.driver.resident_legend_is_hidden("/B/Foo"));

    harness
        .driver
        .click_resident_legend("chart-legend:live-memory-app-resident:/A/Foo:0.000001 MiB");

    assert!(harness.driver.resident_legend_is_hidden("/A/Foo"));
    assert!(!harness.driver.resident_legend_is_hidden("/B/Foo"));
    harness
        .driver
        .click_resident_legend("chart-legend:live-memory-app-resident:/B/Foo:0.000001 MiB");
    assert!(harness.driver.resident_legend_is_hidden("/A/Foo"));
    assert!(harness.driver.resident_legend_is_hidden("/B/Foo"));
    harness
        .driver
        .click_resident_legend("chart-legend:live-memory-app-resident:/A/Foo:0.000001 MiB");
    assert!(!harness.driver.resident_legend_is_hidden("/A/Foo"));
    assert!(harness.driver.resident_legend_is_hidden("/B/Foo"));
    harness.teardown();
}

#[gpui::test]
fn resident_legend_switches_toggle_independently_from_the_keyboard(cx: &mut TestAppContext) {
    // Release Space/Enter on two switches, then restore only the first series.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_interval_resident_leaders();
    harness
        .driver
        .focus_resident_legend("chart-legend:live-memory-app-resident:App A:0.000001 MiB");

    harness.driver.press("space");
    harness.driver.release_legend_key("space");

    assert!(harness.driver.resident_legend_is_hidden("App A"));
    assert!(!harness.driver.resident_legend_is_hidden("App B"));
    harness
        .driver
        .focus_resident_legend("chart-legend:live-memory-app-resident:App B:0.000001 MiB");
    harness.driver.press("enter");
    harness.driver.release_legend_key("enter");
    assert!(harness.driver.resident_legend_is_hidden("App A"));
    assert!(harness.driver.resident_legend_is_hidden("App B"));
    harness
        .driver
        .focus_resident_legend("chart-legend:live-memory-app-resident:App A:0.000001 MiB");
    harness.driver.press("space");
    harness.driver.release_legend_key("space");
    assert!(!harness.driver.resident_legend_is_hidden("App A"));
    assert!(harness.driver.resident_legend_is_hidden("App B"));
    harness.teardown();
}

impl InputDriver {
    fn release_legend_key(&mut self, key: &str) {
        self.context.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
        });
        self.context.run_until_parked();
    }

    fn focus_resident_legend(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).unwrap();
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context.run_until_parked();
    }

    fn click_resident_legend(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).expect(selector);
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }

    fn resident_legend_is_hidden(&self, name: &str) -> bool {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.hidden.contains(&format!(
                "{}\t{name}",
                crate::live_resident_history::PANEL_ID
            ))
        })
    }
}

fn same_basename_resident_history() -> serde_json::Value {
    let mut history = interval_resident_history();
    history["cohort"][0]["key"] = json!({"kind":"bundle",
        "bytes_hex":"2f412f466f6f2e617070","display":"/A/Foo.app"});
    history["cohort"][1]["key"] = json!({"kind":"bundle",
        "bytes_hex":"2f422f466f6f2e617070","display":"/B/Foo.app"});
    history["coverage"]["context_key_bytes"] = json!(55);

    history
}

#[gpui::test]
fn resident_history_reserved_other_label_has_an_independent_app_toggle(cx: &mut TestAppContext) {
    // Keep the captured executable Other distinct from the sum of noncohort Apps.
    let mut harness = InputHarness::setup_route(cx, "memory");
    let mut history = interval_resident_history();
    history["cohort"][0]["key"] = json!({"kind":"executable",
        "bytes_hex":"4f74686572","display":"Other"});
    harness.driver.seed_resident_leaders(history);
    assert!(harness.driver.reading_is_visible("Other (executable)"));

    harness.driver.click_resident_legend(
        "chart-legend:live-memory-app-resident:Other (executable):0.000001 MiB",
    );

    assert!(
        harness
            .driver
            .resident_legend_is_hidden("Other (executable)")
    );
    assert!(!harness.driver.resident_legend_is_hidden("Other"));
    harness.teardown();
}

#[gpui::test]
fn resident_history_identical_display_paths_keep_key_kinds_independent(cx: &mut TestAppContext) {
    // Disambiguate matching displayed paths after two bundle basenames have already collided.
    let mut harness = InputHarness::setup_route(cx, "memory");
    let mut history = same_basename_resident_history();
    history["cohort"][2]["key"] = json!({"kind":"executable",
        "bytes_hex":"2f412f466f6f2e617070","display":"/A/Foo.app"});
    history["coverage"]["context_key_bytes"] = json!(60);
    harness.driver.seed_resident_leaders(history);
    assert!(harness.driver.reading_is_visible("/A/Foo.app (bundle)"));
    assert!(harness.driver.reading_is_visible("/A/Foo.app (executable)"));

    harness.driver.click_resident_legend(
        "chart-legend:live-memory-app-resident:/A/Foo.app (bundle):0.000001 MiB",
    );

    assert!(
        harness
            .driver
            .resident_legend_is_hidden("/A/Foo.app (bundle)")
    );
    assert!(
        !harness
            .driver
            .resident_legend_is_hidden("/A/Foo.app (executable)")
    );
    harness.teardown();
}

#[path = "live_app_attribution_input_tests.rs"]
mod attribution;
