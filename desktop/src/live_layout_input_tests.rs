use gpui::{Bounds, Pixels, TestAppContext, px};
use num_traits::ToPrimitive;
use serde_json::json;

use super::{InputDriver, InputHarness, accepted_snapshot};

#[gpui::test]
fn overview_disk_ranking_activates_the_exact_historical_app(cx: &mut TestAppContext) {
    // Use the installed fourth ranking and its historical App link after real plot input.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_overview_context();
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:80.0 B/s")
    );

    let position = harness
        .driver
        .second_position("live-cpu-plot", 898.5 / 900.0);
    harness
        .driver
        .context
        .simulate_click(position, gpui::Modifiers::none());
    harness.driver.complete_ranked_selection();
    harness
        .driver
        .leave_plot("live-cpu-plot", super::PlotExit::Sidebar);
    harness
        .driver
        .click_ranked_app("ranking:Disk I/O:Historical:20.0 B/s");

    harness.driver.assert_ranked_app_request(999, 1, "42");
    harness.teardown();
}

macro_rules! overview_context_width_case {
    ($name:ident, $width:literal, $assertion:ident) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Compare installed context card bounds at one independently reported viewport width.
            let mut harness = InputHarness::setup_route(cx, "overview");
            harness.driver.seed_overview_context();
            harness.driver.resize_overview_context($width);

            harness.driver.$assertion();

            harness.teardown();
        }
    };
}

overview_context_width_case!(
    overview_context_is_side_by_side_at_1320,
    1320.0,
    assert_overview_context_columns
);
overview_context_width_case!(
    overview_context_is_side_by_side_at_640,
    640.0,
    assert_overview_context_columns
);
overview_context_width_case!(
    overview_context_is_stacked_at_639,
    639.0,
    assert_overview_context_stacked
);

#[gpui::test]
fn overview_context_follows_exact_hover_and_gap_while_live_acceptance_advances(
    cx: &mut TestAppContext,
) {
    // Keep historical identity during a live acceptance, then leave a true gap to restore Live.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_overview_context();

    harness.driver.hover_previous_second_on("live-cpu-plot");
    harness.driver.assert_overview_context([
        "overview-context:soc:40.0 °C",
        "overview-context:fan0:1000 rpm",
        "overview-context:fan1:1100 rpm",
        "overview-context:charge:80.0 %",
    ]);
    harness.driver.advance_overview_context();
    harness.driver.assert_overview_context([
        "overview-context:soc:40.0 °C",
        "overview-context:fan0:1000 rpm",
        "overview-context:fan1:1100 rpm",
        "overview-context:charge:80.0 %",
    ]);
    let gap = harness
        .driver
        .second_position("live-cpu-plot", 896.5 / 900.0);
    harness.driver.hover_position(gap);
    harness.driver.assert_overview_context([
        "overview-context:soc:Unavailable",
        "overview-context:fan0:Unavailable",
        "overview-context:fan1:Unavailable",
        "overview-context:charge:Unavailable",
    ]);
    harness
        .driver
        .leave_plot("live-cpu-plot", super::PlotExit::Sidebar);

    harness.driver.assert_overview_context([
        "overview-context:soc:70.0 °C",
        "overview-context:fan0:3000 rpm",
        "overview-context:fan1:Unavailable",
        "overview-context:charge:45.0 %",
    ]);
    harness.teardown();
}

#[gpui::test]
fn overview_shows_six_primary_charts_before_rankings_and_keeps_resource_context_below(
    cx: &mut TestAppContext,
) {
    // Read the composed 3-by-2 primary grid and the retained contextual plots beneath rankings.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_rolling_selection();
    harness.driver.resize_charts(1320.0);

    harness.driver.assert_overview_primary_grid();

    harness.teardown();
}

#[gpui::test]
fn chart_cards_use_reference_padding_and_eight_pixel_column_gaps(cx: &mut TestAppContext) {
    // Compare production card edges, headline bounds and Data controls in a real chart row.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_rolling_selection();
    harness.driver.resize_charts(1320.0);
    let card = harness.driver.chart_bounds("chart-card:live-cpu");
    let adjacent = harness.driver.chart_bounds("chart-card:live-gpu-activity");
    let next_row = harness.driver.chart_bounds("chart-card:live-power");
    let headline = harness.driver.chart_bounds("chart-reading:live-cpu:0.0 %");
    let data = harness.driver.chart_bounds("chart-data:live-cpu");

    assert_eq!(adjacent.left() - card.right(), px(8.0));
    assert_eq!(next_row.top() - card.bottom(), px(8.0));
    assert_eq!(headline.left() - card.left(), px(13.0));
    assert_eq!(card.right() - headline.right(), px(13.0));
    assert_eq!(data.top() - card.top(), px(9.0));
    harness.teardown();
}

#[gpui::test]
fn memory_charts_and_attribution_precede_the_full_quantity_table(cx: &mut TestAppContext) {
    // Measure the installed page order at both chart layouts without moving its selection.
    let mut harness = InputHarness::setup_route(cx, "memory");
    let selected = harness.driver.accepted_selection().unwrap()["selected"].clone();

    for width in [1320.0, 600.0] {
        harness
            .driver
            .context
            .simulate_resize(gpui::size(px(width), px(4000.0)));
        harness.driver.context.run_until_parked();
        let quantities = harness
            .driver
            .chart_bounds("memory-quantity:Memory pressure:Unavailable");
        let ranking = harness.driver.chart_bounds("ranking-help:Resident memory");
        let history = harness
            .driver
            .chart_bounds("memory-attribution-unavailable");
        let rank_card = harness.driver.chart_bounds("memory-attribution-ranking");

        assert!(harness.driver.chart_bounds("live-memory-plot").bottom() < quantities.top());
        assert!(harness.driver.chart_bounds("live-swap-flow-plot").bottom() < ranking.top());
        assert!(ranking.bottom() < quantities.top());
        if width >= 640.0 {
            assert_eq!(rank_card.left() - history.right(), px(8.0));
        } else {
            assert_eq!(rank_card.top() - history.bottom(), px(8.0));
        }
    }
    assert_eq!(
        harness.driver.accepted_selection().unwrap()["selected"],
        selected
    );
    harness.teardown();
}

#[gpui::test]
fn overview_charts_reflow_when_the_same_window_crosses_both_breakpoints(cx: &mut TestAppContext) {
    // Render one exact 900-second selection while resizing through the approved 3/2/1 layout.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_rolling_selection();
    let selected = harness.driver.accepted_selection().unwrap()["selected"].clone();

    harness.driver.resize_charts(1320.0);
    harness.driver.assert_overview_columns(3);
    harness.driver.resize_charts(1100.0);
    harness.driver.assert_overview_columns(3);
    harness.driver.resize_charts(1099.0);
    harness.driver.assert_overview_columns(2);
    harness.driver.resize_charts(840.0);
    harness.driver.assert_overview_columns(2);
    harness.driver.resize_charts(640.0);
    harness.driver.assert_overview_columns(2);
    harness.driver.resize_charts(639.0);
    harness.driver.assert_overview_columns(1);
    harness.driver.resize_charts(600.0);
    harness.driver.assert_overview_columns(1);
    harness.driver.resize_charts(1320.0);

    harness.driver.assert_overview_columns(3);
    assert_eq!(
        harness.driver.accepted_selection().unwrap()["selected"],
        selected
    );
    harness.teardown();
}

#[gpui::test]
fn regrouping_the_third_overview_chart_ends_hover_outside_the_resized_window(
    cx: &mut TestAppContext,
) {
    // Keep the pointer stationary as the hovered third column moves below the first row.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_rolling_selection();
    harness.driver.resize_charts(1320.0);
    let selected = harness.driver.accepted_selection().unwrap()["selected"].clone();
    harness.driver.hover_previous_second_on("live-memory-plot");
    assert!(harness.driver.cursor_is_visible("live-memory-plot"));
    let pointer = harness.driver.pointer_position();

    harness
        .driver
        .resize_with_retained_pointer(gpui::size(px(840.0), px(2500.0)), pointer);

    assert!(pointer.x > px(840.0));
    assert_eq!(harness.driver.pointer_position(), pointer);
    assert!(!harness.driver.cursor_is_visible("live-memory-plot"));
    assert!(!harness.driver.cursor_is_visible("live-cpu-plot"));
    assert_eq!(
        harness.driver.accepted_selection().unwrap()["selected"],
        selected
    );
    harness.teardown();
}

#[gpui::test]
fn regrouping_a_hovered_first_plot_recomputes_its_reading_at_the_stationary_pointer(
    cx: &mut TestAppContext,
) {
    // Cross from three columns to two while the pointer stays inside the first plot.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_rolling_selection();
    harness.driver.resize_charts(1100.0);
    let selected = harness.driver.accepted_selection().unwrap()["selected"].clone();
    let original = harness.driver.plot_bounds("live-cpu-plot");
    harness.driver.resize_charts(1099.0);
    let resized = harness.driver.plot_bounds("live-cpu-plot");
    let overlap = original.intersect(&resized);
    assert!(overlap.size.height > px(0.0), "{original:?}; {resized:?}");
    let pointer = gpui::point(original.center().x, overlap.center().y);
    assert!(overlap.contains(&pointer));
    harness.driver.resize_charts(1100.0);
    assert_eq!(harness.driver.plot_bounds("live-cpu-plot"), original);
    harness.driver.hover_position(pointer);
    harness.driver.assert_cpu_inspection(551, "51.0 %");

    harness
        .driver
        .resize_with_retained_pointer(gpui::size(px(1099.0), px(2500.0)), pointer);

    let plot = harness.driver.plot_bounds("live-cpu-plot");
    assert!(plot.contains(&pointer));
    assert_eq!(harness.driver.pointer_position(), pointer);
    let fraction = f32::from(pointer.x - plot.left()) / f32::from(plot.size.width);
    let second = 101 + (fraction * 900.0).floor().to_u64().unwrap();
    assert_eq!(second, 366);
    harness.driver.assert_cpu_inspection(second, "66.0 %");
    assert_eq!(
        harness.driver.accepted_selection().unwrap()["selected"],
        selected
    );
    harness.teardown();
}

#[gpui::test]
fn energy_power_and_attribution_precede_thermal_context_at_wide_and_narrow_widths(
    cx: &mut TestAppContext,
) {
    // Measure the installed power pair, captured App plot, ranking and source context in order.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_energy_layout_selection();
    let selected = harness.driver.accepted_selection().unwrap()["selected"].clone();

    harness.driver.resize_charts(1320.0);
    harness.driver.assert_energy_layout(false);
    harness.driver.resize_charts(840.0);
    harness.driver.assert_energy_layout(false);
    harness.driver.resize_charts(640.0);
    harness.driver.assert_energy_layout(false);
    harness.driver.resize_charts(639.0);
    harness.driver.assert_energy_layout(true);
    harness.driver.resize_charts(600.0);
    harness.driver.assert_energy_layout(true);

    assert_eq!(
        harness.driver.accepted_selection().unwrap()["selected"],
        selected
    );
    harness.teardown();
}

#[gpui::test]
fn narrow_headers_keep_selection_controls_inside_the_content(cx: &mut TestAppContext) {
    // Read the production controls' rendered bounds after resizing a paused Overview.
    let mut harness = InputHarness::setup_route(cx, "overview");

    harness.driver.resize_charts(600.0);

    harness
        .driver
        .assert_header_control_visible("open-recording-available");
    harness
        .driver
        .assert_header_control_visible("reading-title");
    harness
        .driver
        .assert_header_control_visible("reading-controls");
    harness.driver.assert_header_control_visible("copy-reading");
    harness
        .driver
        .assert_header_control_visible("inspection-time");
    harness
        .driver
        .assert_header_control_visible("source-follow:Live");
    harness.teardown();
}

impl InputDriver {
    fn seed_overview_context(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.request.second = None;
            dashboard.request.identity = None;
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let points = snapshot.selection["trends"].as_array_mut().unwrap();
            points[0]["soc_celsius"] = json!(40);
            points[0]["fan_rpm"] = json!([1000, 1100]);
            points[0]["battery"] = json!({"charge_percent":80});
            points[1]["soc_celsius"] = json!(60);
            points[1]["fan_rpm"] = json!([2000, 2100]);
            points[1]["battery"] = json!({"charge_percent":50});
            snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
            snapshot.rankings = Some(overview_context_rankings(&snapshot));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.resize_overview_context(1320.0);
    }

    fn resize_overview_context(&mut self, width: f32) {
        self.context
            .simulate_resize(gpui::size(px(width), px(7000.0)));
        self.context.run_until_parked();
    }

    fn assert_overview_context_columns(&mut self) {
        let thermal = self.chart_bounds("overview-context:thermal");
        let cooling = self.chart_bounds("overview-context:cooling-battery");
        assert_eq!(thermal.top(), cooling.top());
        assert_eq!(cooling.left() - thermal.right(), px(8.0));
        self.assert_overview_context_selected();
    }

    fn assert_overview_context_stacked(&mut self) {
        let thermal = self.chart_bounds("overview-context:thermal");
        let cooling = self.chart_bounds("overview-context:cooling-battery");
        assert_eq!(thermal.left(), cooling.left());
        assert_eq!(cooling.top() - thermal.bottom(), px(8.0));
        self.assert_overview_context_selected();
    }

    fn assert_overview_context_selected(&mut self) {
        assert!(self.reading_is_visible("overview-context:thermal:Uncaptured"));
        self.assert_overview_context([
            "overview-context:soc:60.0 °C",
            "overview-context:fan0:2000 rpm",
            "overview-context:fan1:2100 rpm",
            "overview-context:charge:50.0 %",
        ]);
    }

    fn assert_overview_context(&mut self, selectors: [&'static str; 4]) {
        for selector in selectors {
            assert!(self.context.debug_bounds(selector).is_some(), "{selector}");
        }
    }

    fn advance_overview_context(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.selection["second"] = json!(1001);
            snapshot.selection["selected"]["publication"] = json!(3);
            let mut point = snapshot.selection["trends"][1].clone();
            point["publication"] = json!(3);
            point["wall_ns"] = json!(1_001_000_000_000_u64);
            point["soc_celsius"] = json!(70);
            point["fan_rpm"] = json!([3000, null]);
            point["battery"] = json!({"charge_percent":45});
            snapshot.selection["summary"] = point.clone();
            snapshot.selection["trends"]
                .as_array_mut()
                .unwrap()
                .push(point);
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn assert_overview_primary_grid(&mut self) {
        let first = [
            self.chart_bounds("live-cpu-plot"),
            self.chart_bounds("live-gpu-activity-plot"),
            self.chart_bounds("live-memory-plot"),
        ];
        let second = [
            self.chart_bounds("live-power-plot"),
            self.chart_bounds("live-storage-throughput-plot"),
            self.chart_bounds("live-network-plot"),
        ];
        assert_chart_columns(&first, 3);
        assert_chart_columns(&second, 3);
        assert!(first[2].bottom() < second[0].top());
        let ranking = self.context.debug_bounds("ranking-help:CPU").unwrap();
        assert!(second[2].bottom() < ranking.top());
        assert!(self.chart_bounds("live-memory-pressure-plot").top() > ranking.bottom());
        assert!(self.chart_bounds("live-temperature-plot").top() > ranking.bottom());
    }

    fn assert_cpu_inspection(&mut self, second: u64, headline: &'static str) {
        let (observed, accessible) = self.chart_accessibility_reading("live-cpu");
        assert_eq!(observed, Some(second));
        assert!(accessible.contains(&format!("Unix second {second}")));
        assert!(accessible.starts_with(&crate::live_presentation::clock(second, true)));
        assert!(accessible.ends_with(headline));
        assert!(self.context.debug_bounds(headline).is_some());
        let offset = u16::try_from(second - 101).unwrap();
        self.assert_cursor_tracks_fraction("live-cpu-plot", f32::from(offset) / 900.0);
    }

    fn seed_rolling_selection(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            let points: Vec<_> = (101_u64..=1000)
                .map(|second| {
                    json!({"run":snapshot.selection["selected"]["run"],
                        "boot":snapshot.selection["selected"]["boot"],
                        "publication":second,"wall_ns":second * 1_000_000_000,
                        "segment":0,"cpu_percent":second % 100,
                        "power_watts":8,"gpu_watts":2,"soc_celsius":40})
                })
                .collect();
            snapshot.selection["selected"]["publication"] = json!(1000);
            snapshot.selection["trends"] = json!(points);
            dashboard.request.identity = Some(snapshot.selection["selected"].clone());
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn resize_charts(&mut self, width: f32) {
        self.context
            .simulate_resize(gpui::size(px(width), px(2500.0)));
        self.context.run_until_parked();
    }

    fn assert_overview_columns(&mut self, columns: usize) {
        let bounds = [
            self.chart_bounds("live-cpu-plot"),
            self.chart_bounds("live-gpu-activity-plot"),
            self.chart_bounds("live-memory-plot"),
        ];
        assert_chart_columns(&bounds, columns);
    }

    fn seed_energy_layout_selection(&mut self) {
        use crate::live_app_attribution_history::{AppAttributionHistory, Metric, tests::fixture};
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            for (index, point) in snapshot.selection["trends"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .enumerate()
            {
                point["soc_celsius"] = json!([20.0, 40.0][index]);
                point["fan_rpm"] = json!([[1000.0], [1200.0]][index]);
            }
            snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
            let history = AppAttributionHistory::prepare(
                Metric::Power,
                &fixture(Metric::Power),
                &snapshot.selection,
            );
            assert!(history.panel().is_some(), "{}", history.coverage(None));
            snapshot.app_attribution_history = Some(std::sync::Arc::new(history));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn assert_energy_layout(&mut self, narrow: bool) {
        let system = self.chart_bounds("chart-card:live-energy");
        let gpu = self.chart_bounds("chart-card:live-gpu-power");
        let attribution = self.chart_bounds("attribution-row:live-power-apps");
        let apps = self.chart_bounds("chart-card:live-power-apps");
        let ranking = self.chart_bounds("attribution-ranking:live-power-apps");
        let temperature = self.chart_bounds("chart-card:live-soc-temperature");
        let fans = self.chart_bounds("chart-card:live-fans");
        if narrow {
            assert_eq!(gpu.top() - system.bottom(), px(8.0));
            assert_eq!(ranking.top() - apps.bottom(), px(8.0));
            assert_eq!(fans.top() - temperature.bottom(), px(8.0));
        } else {
            assert_eq!(system.top(), gpu.top());
            assert_eq!(gpu.left() - system.right(), px(8.0));
            assert_eq!(ranking.left() - apps.right(), px(8.0));
            assert_eq!(temperature.top(), fans.top());
            assert_eq!(fans.left() - temperature.right(), px(8.0));
        }
        assert!(gpu.bottom() < attribution.top());
        assert!(attribution.bottom() < temperature.top());
    }

    fn assert_header_control_visible(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).expect(selector);
        assert!(bounds.left() >= px(170.0), "{selector}: {bounds:?}");
        assert!(bounds.right() <= px(576.0), "{selector}: {bounds:?}");
        assert!(bounds.size.width > px(0.0), "{selector}: {bounds:?}");
    }
}

fn overview_context_rankings(
    snapshot: &crate::live_source::Snapshot,
) -> std::sync::Arc<crate::live_rankings::Rankings> {
    let measure = json!({"known":"80","measured_members":1,
        "unavailable_members":0,"overflowed":false});
    let app = json!({"display_name":"Current","key":{"kind":"bundle","bytes_hex":"41"},
        "cpu_ns":measure,"energy_nj":measure,"resident_bytes":measure,
        "disk_read_bytes":measure,"disk_written_bytes":measure,"disk_io_bytes":measure,
        "cpu_percent_one_core":80,"energy_watts":0.5,"disk_io_bps":80.0});
    let page = json!({"selected":snapshot.selection["selected"],"apps":[app],
        "app_count":1,"membership_complete":true,"ungrouped_members":0,
        "page":{"offset":0,"limit":512,"total":1,"next_offset":null}});
    std::sync::Arc::new(crate::live_rankings::read(
        &snapshot.selection["selected"],
        |_| Ok(page.clone()),
    ))
}

fn assert_chart_columns(bounds: &[Bounds<Pixels>; 3], columns: usize) {
    let first_row = bounds
        .iter()
        .take_while(|chart| chart.top() == bounds[0].top())
        .count();
    assert_eq!(first_row, columns, "Rendered chart bounds: {bounds:?}");
    for adjacent in bounds.windows(2) {
        if adjacent[0].top() == adjacent[1].top() {
            assert!((adjacent[0].size.width - adjacent[1].size.width).abs() <= px(1.0));
            assert!(adjacent[0].right() < adjacent[1].left());
        } else {
            assert!(adjacent[0].bottom() < adjacent[1].top());
        }
    }
}
