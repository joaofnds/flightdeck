use gpui::{Bounds, Pixels, TestAppContext, px};
use num_traits::ToPrimitive;
use serde_json::json;

use super::{InputDriver, InputHarness, accepted_snapshot};

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
fn energy_charts_keep_two_columns_until_the_narrow_breakpoint(cx: &mut TestAppContext) {
    // A resource page has two columns even at the width where Overview has three.
    let mut harness = InputHarness::setup(cx);
    harness.driver.seed_rolling_selection();

    harness.driver.resize_charts(1320.0);
    harness.driver.assert_energy_columns(2);
    harness.driver.resize_charts(840.0);
    harness.driver.assert_energy_columns(2);
    harness.driver.resize_charts(600.0);

    harness.driver.assert_energy_columns(1);
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

    fn assert_energy_columns(&mut self, columns: usize) {
        let bounds = [
            self.chart_bounds("live-energy-plot"),
            self.chart_bounds("live-soc-temperature-plot"),
            self.chart_bounds("live-fans-plot"),
        ];
        assert_chart_columns(&bounds, columns);
    }

    fn assert_header_control_visible(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).expect(selector);
        assert!(bounds.left() >= px(170.0), "{selector}: {bounds:?}");
        assert!(bounds.right() <= px(576.0), "{selector}: {bounds:?}");
        assert!(bounds.size.width > px(0.0), "{selector}: {bounds:?}");
    }
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
