use super::*;
use crate::live_app_attribution_history::{AppAttributionHistory, Metric, tests::fixture};
use crate::live_overview_app_histories::OverviewAppHistories;

#[path = "live_app_machine_reference_input_tests.rs"]
mod machine_reference;

macro_rules! overview_history_width_case {
    ($name:ident, $width:literal, $assertion:ident) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Observe independently virtualized history rows at one installed viewport width.
            let mut harness = InputHarness::setup_route(cx, "overview");
            harness.driver.seed_overview_histories($width);

            $assertion(&mut harness.driver);
            assert_overview_histories_order(&mut harness.driver);

            harness.teardown();
        }
    };
}

overview_history_width_case!(
    overview_app_histories_pair_at_1100,
    1100.0,
    assert_overview_histories_paired
);
overview_history_width_case!(
    overview_app_histories_stack_at_639,
    639.0,
    assert_overview_histories_stacked
);

macro_rules! overview_history_hover_case {
    ($name:ident, $plot:literal, $tooltip:literal) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // A real plot event synchronizes four histories before and after exact details.
            let mut harness = InputHarness::setup_route(cx, "overview");
            harness.driver.seed_overview_histories(1100.0);
            let bounds = harness.driver.chart_bounds($plot);

            harness.driver.hover_previous_second_on($plot);

            assert_overview_histories_historical(&mut harness.driver);
            assert!(harness.driver.reading_is_visible($tooltip));
            assert!(
                harness
                    .driver
                    .reading_is_visible("Loading exact App totals")
            );
            harness.driver.complete_overview_history_preview();
            assert_overview_histories_historical(&mut harness.driver);
            assert!(
                harness
                    .driver
                    .reading_is_visible("ranking:CPU:Historical:20.0%")
            );
            assert_eq!(harness.driver.chart_bounds($plot), bounds);
            harness.driver.leave_plot($plot, PlotExit::Sidebar);
            assert_overview_histories_current(&mut harness.driver);
            assert!(!harness.driver.cursor_is_visible($plot));
            assert_eq!(harness.driver.chart_bounds($plot), bounds);
            harness.teardown();
        }
    };
}

overview_history_hover_case!(
    overview_cpu_history_synchronizes_exact_readings,
    "live-cpu-apps-plot",
    "chart-tooltip-row:live-cpu-apps:0:App A:120%"
);
overview_history_hover_case!(
    overview_resident_history_synchronizes_exact_readings,
    "live-memory-app-resident-plot",
    "chart-tooltip-row:live-memory-app-resident:0:Resident App:1 GiB"
);
overview_history_hover_case!(
    overview_disk_history_synchronizes_exact_readings,
    "live-disk-apps-plot",
    "chart-tooltip-row:live-disk-apps:0:Current:500 kB/s"
);
overview_history_hover_case!(
    overview_power_history_synchronizes_exact_readings,
    "live-power-apps-plot",
    "chart-tooltip-row:live-power-apps:0:App A:1.2 W"
);

#[gpui::test]
fn overview_history_failure_preserves_captured_readings_until_plot_exit(cx: &mut TestAppContext) {
    // Failed exact details withhold current rankings while the captured cohort remains readable.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_overview_histories(1100.0);
    harness
        .driver
        .hover_previous_second_on("live-power-apps-plot");

    harness.driver.fail_ranked_preview();

    assert_overview_histories_historical(&mut harness.driver);
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
    harness
        .driver
        .leave_plot("live-power-apps-plot", PlotExit::Sidebar);
    assert_overview_histories_current(&mut harness.driver);
    harness.teardown();
}

#[gpui::test]
fn overview_history_gap_withholds_all_four_selected_readings(cx: &mut TestAppContext) {
    // An unobserved second must not inherit any of the accepted four App histories' readings.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_overview_histories(1100.0);
    let gap = harness
        .driver
        .second_position("live-disk-apps-plot", 897.5 / 900.0);

    harness.driver.hover_position(gap);
    harness.driver.finish_missing_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-cpu-apps:Unavailable")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-memory-app-resident:Unavailable")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-disk-apps:Unavailable")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-power-apps:Unavailable")
    );
    harness.teardown();
}

#[gpui::test]
fn overview_late_history_details_never_replace_the_current_hover(cx: &mut TestAppContext) {
    // An earlier request generation cannot install App totals in the four accepted histories.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_overview_histories(1100.0);
    harness
        .driver
        .hover_previous_second_on("live-cpu-apps-plot");

    harness.driver.deliver_late_overview_history_details();

    assert_overview_histories_historical(&mut harness.driver);
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
    harness.teardown();
}

#[gpui::test]
fn overview_wrong_observation_details_preserve_captured_hover_readings(cx: &mut TestAppContext) {
    // A response at the expected second but wrong publication fails locally before install.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_overview_histories(1100.0);
    harness
        .driver
        .hover_previous_second_on("live-disk-apps-plot");

    harness.driver.deliver_wrong_overview_history_details();

    assert_overview_histories_historical(&mut harness.driver);
    assert!(
        harness.driver.reading_is_visible(
            "App totals unavailable: Hover response has a different observation"
        )
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0%")
    );
    harness.teardown();
}

#[gpui::test]
fn overview_one_unavailable_history_keeps_the_other_three_cards(cx: &mut TestAppContext) {
    // Refuse only Power's captured context while CPU, resident and disk remain installed.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_overview_histories(1100.0);

    harness.driver.refuse_overview_power_history();

    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-cpu-apps:250% captured")
    );
    assert!(harness.driver.reading_is_visible(
        "chart-reading:live-memory-app-resident:3221225472 B captured resident"
    ));
    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-disk-apps:1.5 MB/s captured")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("App rate history unavailable: Power read failed")
    );
    harness.teardown();
}

#[gpui::test]
fn overview_history_data_and_legend_keep_each_resource_reading(cx: &mut TestAppContext) {
    // Installed Data controls and legend toggles must select the Disk adapter in the cohort group.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_overview_histories(1100.0);

    harness
        .driver
        .click_resident_legend("chart-data:live-disk-apps");

    assert!(harness.driver.reading_is_visible("1.5 MB/s"));
    assert!(harness.driver.reading_is_visible("Available samples"));
    harness.driver.click_resident_legend("chart-data-close");
    harness
        .driver
        .leave_plot("live-disk-apps-plot", PlotExit::Sidebar);
    harness
        .driver
        .focus_resident_legend("chart-legend:live-disk-apps:Current:1.5 MB/s");
    harness.driver.press("space");
    harness.driver.release_legend_key("space");
    harness
        .driver
        .click_resident_legend("chart-data:live-disk-apps");
    assert!(!harness.driver.reading_is_visible("1.5 MB/s"));
    harness.teardown();
}

/// The card of one history slot: its chart, or the card that says it is not recorded.
fn history_card(driver: &mut InputDriver, id: &'static str) -> gpui::Bounds<gpui::Pixels> {
    // Selectors are static strings, so the few this builds are leaked for the test's life.
    let chart: &'static str = Box::leak(format!("chart-card:{id}").into_boxed_str());
    let missing: &'static str =
        Box::leak(format!("overview-app-history-unavailable:{id}").into_boxed_str());
    driver
        .context
        .debug_bounds(chart)
        .or_else(|| driver.context.debug_bounds(missing))
        .unwrap_or_else(|| panic!("{id}"))
}

fn history_cards(driver: &mut InputDriver) -> [gpui::Bounds<gpui::Pixels>; 6] {
    [0, 1, 2, 3, 4, 5].map(|slot| {
        history_card(
            driver,
            crate::live_overview_app_histories::overview_app_history_slot(slot).0,
        )
    })
}

fn assert_overview_histories_paired(driver: &mut InputDriver) {
    let cards = history_cards(driver);
    for pair in cards.chunks(2) {
        assert_eq!(pair[0].top(), pair[1].top());
        assert_eq!(pair[1].left() - pair[0].right(), px(8.0));
    }
    assert!(cards[0].bottom() < cards[2].top());
    assert!(cards[2].bottom() < cards[4].top());
}

fn assert_overview_histories_stacked(driver: &mut InputDriver) {
    let cards = history_cards(driver);
    for pair in cards.windows(2) {
        assert_eq!(pair[0].left(), pair[1].left());
        assert!(pair[0].bottom() < pair[1].top());
    }
}

fn assert_overview_histories_order(driver: &mut InputDriver) {
    let top = driver.chart_bounds("Top apps at the selected time");
    let heading = driver.chart_bounds("App resource histories");
    let cards = history_cards(driver);
    let context = driver.chart_bounds("More resource context");
    let macbook = driver.chart_bounds("MacBook");
    assert!(top.bottom() < heading.top());
    assert!(heading.bottom() < cards[0].top());
    assert!(cards[5].bottom() < context.top());
    assert!(context.bottom() < macbook.top());
    driver.view.read_with(&driver.context, |dashboard, _| {
        let slots: Vec<_> = dashboard
            .rows
            .iter()
            .filter_map(|row| {
                if let crate::live_dashboard::Row::OverviewAppHistories(slots) = row {
                    Some(slots.as_slice())
                } else {
                    None
                }
            })
            .flatten()
            .copied()
            .collect();
        assert_eq!(slots, [0, 1, 2, 3, 4, 5]);
    });
}

fn assert_overview_histories_historical(driver: &mut InputDriver) {
    assert!(driver.reading_is_visible("chart-reading:live-cpu-apps:130% captured"));
    assert!(driver.reading_is_visible(
        "chart-reading:live-memory-app-resident:1073741824 B captured resident"
    ));
    assert!(driver.reading_is_visible("chart-reading:live-disk-apps:500 kB/s captured"));
    assert!(driver.reading_is_visible("chart-reading:live-power-apps:1.5 W captured"));
    assert!(driver.reading_is_visible("chart-legend:live-cpu-apps:App A:120%"));
    assert!(driver.reading_is_visible("chart-legend:live-power-apps:App A:1.2 W"));
    assert!(driver.reading_is_visible("13%"));
}

fn assert_overview_histories_current(driver: &mut InputDriver) {
    assert!(driver.reading_is_visible("chart-reading:live-cpu-apps:250% captured"));
    assert!(driver.reading_is_visible(
        "chart-reading:live-memory-app-resident:3221225472 B captured resident"
    ));
    assert!(driver.reading_is_visible("chart-reading:live-disk-apps:1.5 MB/s captured"));
    assert!(driver.reading_is_visible("chart-reading:live-power-apps:2.8 W captured"));
    assert!(driver.reading_is_visible("ranking:CPU:Current:80.0%"));
}

impl InputDriver {
    fn seed_overview_histories(&mut self, width: f32) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = overview_history_snapshot(dashboard.generation);
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(width), px(12000.0)));
        self.context.run_until_parked();
    }

    fn complete_overview_history_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick_at(Instant::now(), cx);
            let preview = dashboard.preview.as_ref().unwrap();
            assert!(!preview.request.load_app_attribution_history);
            assert!(!preview.request.load_resident_history);
            let histories = Arc::clone(preview.reading.overview_app_histories.as_ref().unwrap());
            let resident = Arc::clone(preview.reading.resident_history.as_ref().unwrap());
            let mut snapshot = route_ranked_snapshot(preview.generation.unwrap());
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = preview.target.identity.clone().unwrap();
            snapshot.selection["summary"] = snapshot.selection["trends"][0].clone();
            snapshot.rankings = Some(route_ranked_payload(&snapshot, RouteReading::Historical));
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            let preview = dashboard.preview.as_ref().unwrap();
            assert!(Arc::ptr_eq(
                &histories,
                preview.reading.overview_app_histories.as_ref().unwrap()
            ));
            assert!(Arc::ptr_eq(
                &resident,
                preview.reading.resident_history.as_ref().unwrap()
            ));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn refuse_overview_power_history(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = overview_history_snapshot(dashboard.generation);
            Arc::get_mut(snapshot.overview_app_histories.as_mut().unwrap())
                .unwrap()
                .power = Arc::new(AppAttributionHistory::Unavailable(
                "Power read failed".into(),
            ));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn deliver_late_overview_history_details(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick_at(Instant::now(), cx);
            let preview = dashboard.preview.as_ref().unwrap();
            let mut snapshot = route_ranked_snapshot(preview.generation.unwrap() + 1);
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = preview.target.identity.clone().unwrap();
            snapshot.rankings = Some(route_ranked_payload(&snapshot, RouteReading::Historical));
            assert!(!dashboard.preview_receive(Update::Ready(snapshot)));
            assert!(dashboard.preview.as_ref().unwrap().failure.is_none());
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn deliver_wrong_overview_history_details(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick_at(Instant::now(), cx);
            let preview = dashboard.preview.as_ref().unwrap();
            let mut snapshot = route_ranked_snapshot(preview.generation.unwrap());
            snapshot.selection["second"] = json!(999);
            snapshot.rankings = Some(route_ranked_payload(&snapshot, RouteReading::Current));
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            assert_eq!(
                dashboard.preview.as_ref().unwrap().failure.as_deref(),
                Some("Hover response has a different observation")
            );
            cx.notify();
        });
        self.context.run_until_parked();
    }
}

fn overview_history_snapshot(generation: u64) -> Box<Snapshot> {
    let mut snapshot = route_ranked_snapshot(generation);
    snapshot.selection["trends"][0]["cpu_percent"] = json!(13);
    snapshot.selection["trends"][1]["cpu_percent"] = json!(87);
    snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
    snapshot.rankings = Some(route_ranked_payload(&snapshot, RouteReading::Current));
    snapshot.overview_app_histories = Some(Arc::new(OverviewAppHistories {
        cpu: Arc::new(AppAttributionHistory::prepare(
            Metric::Cpu,
            &fixture(Metric::Cpu),
            &snapshot.selection,
        )),
        power: Arc::new(AppAttributionHistory::prepare(
            Metric::Power,
            &fixture(Metric::Power),
            &snapshot.selection,
        )),
        disk: Arc::new(AppAttributionHistory::prepare(
            Metric::Disk,
            &crate::live_app_attribution_history::tests::disk_fixture(),
            &snapshot.selection,
        )),
    }));
    snapshot.resident_history = Some(Arc::new(
        crate::live_resident_history::ResidentHistory::prepare(
            &overview_resident_fixture(),
            &snapshot.selection,
        ),
    ));

    snapshot
}

fn overview_resident_fixture() -> serde_json::Value {
    let mut value = fixture(Metric::Cpu);
    value["cohort"] = json!([{"key":{"kind":"executable","bytes_hex":"41",
        "display":"Resident App"},"sum_bytes":"4294967296","samples":2}]);
    for (index, point) in value["points"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        point["apps"] = json!([{"known":([1_073_741_824_u64,3_221_225_472][index]),
            "measured_members":1,"unavailable_members":0,"overflowed":false}]);
        point["other"] = json!({"app_count":0,"resident_bytes":{"known":0,
            "measured_members":0,"unavailable_members":0,"overflowed":false}});
    }

    value
}
