use std::sync::Arc;

use gpui::{TestAppContext, px};
use serde_json::json;

use super::{InputDriver, InputHarness, PlotExit, accepted_snapshot};
use crate::live_rankings::Rankings;
use crate::live_source::Snapshot;

#[test]
fn disk_ranking_preserves_jointly_measured_interval_bytes_and_rate() {
    // Fold a two-second interval so the combined exact byte amount differs from its rate.
    let snapshot = accepted_snapshot(1);

    let rankings = disk_rankings(&snapshot);

    let Rankings::Ready(rankings) = rankings else {
        panic!("{rankings:?}");
    };
    let disk = rankings
        .metrics
        .iter()
        .find(|ranking| ranking.metric.title() == "Disk I/O")
        .expect("Disk I/O ranking missing");
    assert_eq!(rankings.selected, snapshot.selection["selected"]);
    assert_eq!(disk.entries.len(), 1);
    assert_eq!(disk.entries[0].app.name, "Current");
    assert_eq!(disk.entries[0].amount, 3_000_000);
    assert_eq!(disk.entries[0].rate, Some(1_500_000.0));
    assert_eq!(disk.entries[0].measured_members, 1);
    assert_eq!(disk.entries[0].unavailable_members, 0);
}

#[gpui::test]
fn storage_disk_ranking_displays_the_selected_observation_interval(cx: &mut TestAppContext) {
    // Render the production Storage route from one bounded App page with a two-second rate.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.teardown();
}

#[gpui::test]
fn storage_hover_waits_for_exact_historical_app_details(cx: &mut TestAppContext) {
    // Dispatch through the installed Storage graph before any exact App preview is delivered.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();

    harness
        .driver
        .hover_previous_second_on("live-storage-throughput-plot");

    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.teardown();
}

#[gpui::test]
fn storage_disk_hover_uses_exact_ranks_and_restores_live_on_exit(cx: &mut TestAppContext) {
    // Deliver a different historical disk total through the graph's actual hover request.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness
        .driver
        .hover_previous_second_on("live-storage-throughput-plot");

    harness.driver.complete_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Historical:20 B/s")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness
        .driver
        .leave_plot("live-storage-throughput-plot", PlotExit::Sidebar);
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Historical:20 B/s")
    );
    harness.teardown();
}

#[gpui::test]
fn storage_disk_link_keeps_its_keyboard_selected_observation(cx: &mut TestAppContext) {
    // Pause through the installed Storage chart key handler, then open the displayed App rank.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness
        .driver
        .inspect_ranked_history_with_keyboard_on("live-storage-throughput-plot");
    harness.driver.complete_ranked_selection();

    harness
        .driver
        .click_ranked_app("ranking:Disk I/O:Historical:20 B/s");

    harness.driver.assert_ranked_app_request(999, 1, "42");
    harness.teardown();
}

#[gpui::test]
fn storage_disk_reader_failure_never_exposes_live_links(cx: &mut TestAppContext) {
    // A failed exact historical worker read cannot substitute the current App's disk reading.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness
        .driver
        .hover_previous_second_on("live-storage-throughput-plot");

    harness.driver.fail_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: Ranking read failed")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.teardown();
}

#[gpui::test]
fn storage_disk_rank_failure_never_exposes_live_links(cx: &mut TestAppContext) {
    // Scalar history can arrive successfully while only its exact App totals fail.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness
        .driver
        .hover_previous_second_on("live-storage-throughput-plot");

    harness.driver.complete_unavailable_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: App response bound")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.teardown();
}

#[gpui::test]
fn storage_disk_gap_has_no_current_app_link(cx: &mut TestAppContext) {
    // A second without an observation has no App identity to use for disk attribution.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    let position = harness
        .driver
        .second_position("live-storage-throughput-plot", 897.5 / 900.0);

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
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.teardown();
}

impl InputDriver {
    fn seed_disk_storage(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            use super::resource::{ResourceEpoch, resource_devices};
            use crate::live_app_attribution_history::{
                AppAttributionHistory, Metric, tests::disk_fixture,
            };
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.selection["trends"][0]["storage"] = json!({"state":"complete",
                "read_bps":3_000_000.0,"write_bps":1_500_000.0,
                "devices":resource_devices(ResourceEpoch::Historical)});
            snapshot.selection["trends"][1]["storage"] = json!({"state":"complete",
                "read_bps":8_000_000.0,"write_bps":4_000_000.0,
                "devices":resource_devices(ResourceEpoch::Current)});
            snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
            let history =
                AppAttributionHistory::prepare(Metric::Disk, &disk_fixture(), &snapshot.selection);
            assert!(history.panel().is_some(), "{}", history.coverage(None));
            snapshot.app_attribution_history = Some(Arc::new(history));
            snapshot.rankings = Some(Arc::new(disk_rankings(&snapshot)));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(3500.0)));
        self.context.run_until_parked();
    }
}

fn disk_rankings(snapshot: &Snapshot) -> Rankings {
    let measure = |known: u64| {
        json!({"known":known.to_string(),"measured_members":1,
            "unavailable_members":0,"overflowed":false})
    };
    let app = json!({"display_name":"Current","key":{"kind":"bundle","bytes_hex":"41"},
        "cpu_ns":measure(1_000_000),"resident_bytes":measure(1024),
        "energy_nj":measure(1_000_000_000),"cpu_percent_one_core":0.05,"energy_watts":0.5,
        "disk_read_bytes":measure(2_000_000),"disk_written_bytes":measure(1_000_000),
        "disk_io_bytes":measure(3_000_000),"disk_read_bps":1_000_000.0,
        "disk_written_bps":500_000.0,"disk_io_bps":1_500_000.0});
    let page = json!({"selected":snapshot.selection["selected"],"elapsed_ns":"2000000000",
        "apps":[app],"app_count":1,"membership_complete":true,"ungrouped_members":0,
        "page":{"offset":0,"limit":512,"total":1,"next_offset":null}});

    crate::live_rankings::read(&snapshot.selection["selected"], |_| Ok(page.clone()))
}

#[gpui::test]
fn disk_attribution_precedes_devices_and_keeps_the_responsive_ranking_pair(
    cx: &mut TestAppContext,
) {
    // Read installed cards and physical-device sections across the exact shared breakpoint.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness.driver.assert_disk_attribution_layout(false);

    harness.driver.resize_disk_attribution(640.0);
    harness.driver.assert_disk_attribution_layout(false);
    harness.driver.resize_disk_attribution(639.0);
    harness.driver.assert_disk_attribution_layout(true);
    harness.teardown();
}

#[gpui::test]
fn disk_attribution_hover_updates_devices_and_waits_for_exact_rankings(cx: &mut TestAppContext) {
    // The captured App plot drives the whole Storage page while exact details arrive separately.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    let retained = harness.driver.disk_attribution_anchor();

    harness
        .driver
        .hover_previous_second_on("live-disk-apps-plot");

    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-disk-apps:Current:500 kB/s")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Disk read:3 MB/s")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("resource-device:historical-disk0")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.driver.complete_disk_attribution_preview();
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:500 kB/s")
    );
    harness.driver.assert_disk_attribution_anchor(&retained);

    harness
        .driver
        .leave_plot("live-disk-apps-plot", PlotExit::Sidebar);

    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-disk-apps:Current:1.5 MB/s")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("resource-device:current-disk")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.driver.assert_disk_attribution_anchor(&retained);
    harness.teardown();
}

#[gpui::test]
fn disk_attribution_failure_keeps_the_captured_rate_without_current_rank_links(
    cx: &mut TestAppContext,
) {
    // An unavailable exact worker response cannot replace the historical App interval.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness
        .driver
        .hover_previous_second_on("live-disk-apps-plot");

    harness.driver.fail_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-disk-apps:Current:500 kB/s")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: Ranking read failed")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.teardown();
}

#[gpui::test]
fn disk_attribution_gap_clears_the_rate_and_plot_exit_restores_it(cx: &mut TestAppContext) {
    // An unobserved second has no App rate or device summary to borrow from another tick.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness
        .driver
        .hover_previous_second_on("live-disk-apps-plot");
    let gap = harness
        .driver
        .second_position("live-disk-apps-plot", 897.5 / 900.0);

    harness.driver.hover_position(gap);
    harness.driver.finish_missing_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-disk-apps:Current:Unavailable")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("resource-device:current-disk")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("resource-device:historical-disk0")
    );
    harness
        .driver
        .leave_plot("live-disk-apps-plot", PlotExit::Sidebar);
    assert!(
        !harness
            .driver
            .reading_is_visible("chart-tooltip:live-disk-apps")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-disk-apps:Current:1.5 MB/s")
    );
    harness.teardown();
}

#[gpui::test]
fn disk_attribution_tooltip_orders_exact_bytes_when_plot_values_tie(cx: &mut TestAppContext) {
    // Adjacent counts beyond f64 integer precision remain independently sortable in the tooltip.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness.driver.seed_large_disk_attribution();

    harness
        .driver
        .hover_selected_second_on("live-disk-apps-plot");

    assert!(
        harness
            .driver
            .reading_is_visible("chart-tooltip-row:live-disk-apps:0:Higher:4,503,599,616 MB/s")
    );
    harness.teardown();
}

#[gpui::test]
fn disk_attribution_arrow_inspection_preserves_the_ranked_app_target(cx: &mut TestAppContext) {
    // Activate the App reached through the actual new plot's historical keyboard inspection.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness
        .driver
        .inspect_ranked_history_with_keyboard_on("live-disk-apps-plot");
    harness.driver.complete_ranked_selection();

    harness
        .driver
        .click_ranked_app("ranking:Disk I/O:Historical:20 B/s");

    harness.driver.assert_ranked_app_request(999, 1, "42");
    harness.teardown();
}

impl InputDriver {
    fn complete_disk_attribution_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick_at(std::time::Instant::now(), cx);
            let preview = dashboard.preview.as_ref().unwrap();
            assert!(!preview.request.load_app_attribution_history);
            let mut snapshot = accepted_snapshot(preview.generation.unwrap());
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = preview.target.identity.clone().unwrap();
            snapshot.selection["summary"] = preview.reading.selection["summary"].clone();
            let Rankings::Ready(mut rankings) = disk_rankings(&snapshot) else {
                panic!("Disk fixture requires measured rankings");
            };
            rankings.metrics[3].entries[0].amount = 1_000_000;
            rankings.metrics[3].entries[0].rate = Some(500_000.0);
            snapshot.rankings = Some(Arc::new(Rankings::Ready(rankings)));
            assert!(dashboard.preview_receive(crate::live_source::Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn resize_disk_attribution(&mut self, width: f32) {
        self.context
            .simulate_resize(gpui::size(px(width), px(5000.0)));
        self.context.run_until_parked();
    }

    fn assert_disk_attribution_layout(&mut self, narrow: bool) {
        let machine = self.chart_bounds("chart-card:live-storage-errors");
        let row = self.chart_bounds("attribution-row:live-disk-apps");
        let history = self.chart_bounds("chart-card:live-disk-apps");
        let ranking = self.chart_bounds("attribution-ranking:live-disk-apps");
        let device = self.chart_bounds("resource-device:current-disk");
        assert!(machine.bottom() < row.top());
        assert!(row.bottom() < device.top());
        if narrow {
            assert_eq!(ranking.top() - history.bottom(), px(8.0));
        } else {
            assert_eq!(ranking.left() - history.right(), px(8.0));
            assert_eq!(ranking.top(), history.top());
        }
    }

    fn disk_attribution_anchor(
        &self,
    ) -> Arc<crate::live_app_attribution_history::AppAttributionHistory> {
        self.view.read_with(&self.context, |dashboard, _| {
            Arc::clone(
                dashboard
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .app_attribution_history
                    .as_ref()
                    .unwrap(),
            )
        })
    }

    fn assert_disk_attribution_anchor(
        &self,
        retained: &Arc<crate::live_app_attribution_history::AppAttributionHistory>,
    ) {
        assert!(Arc::ptr_eq(retained, &self.disk_attribution_anchor()));
    }

    fn seed_large_disk_attribution(&mut self) {
        use crate::live_app_attribution_history::{
            AppAttributionHistory, Metric, tests::disk_fixture,
        };
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let mut history = disk_fixture();
            history["cohort"] = json!([
                {"key":{"kind":"bundle","bytes_hex":"41","display":"Lower"}},
                {"key":{"kind":"bundle","bytes_hex":"42","display":"Higher"}}]);
            for point in history["points"].as_array_mut().unwrap() {
                let mut higher = point["apps"][0].clone();
                point["apps"][0]["disk_io_bytes"]["known"] = json!("9007199254740992");
                higher["disk_io_bytes"]["known"] = json!("9007199254740993");
                higher["disk_io_bps"] = json!(4_503_599_627_370_496.0);
                point["apps"][0]["disk_io_bps"] = higher["disk_io_bps"].clone();
                point["apps"].as_array_mut().unwrap().push(higher);
            }
            snapshot.app_attribution_history = Some(Arc::new(AppAttributionHistory::prepare(
                Metric::Disk,
                &history,
                &snapshot.selection,
            )));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }
}

#[gpui::test]
fn disk_attribution_data_keeps_tiny_rates_and_native_legend_switches(cx: &mut TestAppContext) {
    // Inspect nonzero byte-rate statistics, then hide and restore the App through native keys.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness.driver.seed_small_disk_attribution();
    harness
        .driver
        .disk_chart_control("chart-data:live-disk-apps");
    assert!(harness.driver.reading_is_visible("10 B/s"));
    assert!(harness.driver.reading_is_visible("10 B/s"));
    harness.driver.disk_chart_control("chart-data-close");
    harness.driver.focus_disk_legend();

    harness.driver.press("space");
    harness.driver.release_disk_legend_key("space");
    harness
        .driver
        .disk_chart_control("chart-data:live-disk-apps");

    assert!(!harness.driver.reading_is_visible("10 B/s"));
    harness.driver.disk_chart_control("chart-data-close");
    harness.driver.focus_disk_legend();
    harness.driver.press("enter");
    harness.driver.release_disk_legend_key("enter");
    harness
        .driver
        .disk_chart_control("chart-data:live-disk-apps");
    assert!(harness.driver.reading_is_visible("10 B/s"));
    assert!(harness.driver.reading_is_visible("2 / 2"));
    harness.teardown();
}

impl InputDriver {
    fn seed_small_disk_attribution(&mut self) {
        use crate::live_app_attribution_history::{
            AppAttributionHistory, Metric, tests::disk_fixture,
        };
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let mut history = disk_fixture();
            for point in history["points"].as_array_mut().unwrap() {
                point["apps"][0]["disk_io_bytes"]["known"] = json!("20");
                point["apps"][0]["disk_io_bps"] = json!(10.0);
            }
            snapshot.app_attribution_history = Some(Arc::new(AppAttributionHistory::prepare(
                Metric::Disk,
                &history,
                &snapshot.selection,
            )));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn disk_chart_control(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).unwrap();
        self.context
            .simulate_click(bounds.center(), gpui::Modifiers::none());
        self.context.run_until_parked();
    }

    fn focus_disk_legend(&mut self) {
        let bounds = self
            .context
            .debug_bounds("chart-legend:live-disk-apps:Current:10 B/s")
            .unwrap();
        self.context.simulate_mouse_down(
            bounds.center(),
            gpui::MouseButton::Right,
            gpui::Modifiers::none(),
        );
        self.context.simulate_mouse_up(
            bounds.center(),
            gpui::MouseButton::Right,
            gpui::Modifiers::none(),
        );
        self.context.run_until_parked();
    }

    fn release_disk_legend_key(&mut self, key: &str) {
        self.context.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
        });
        self.context.run_until_parked();
    }
}
