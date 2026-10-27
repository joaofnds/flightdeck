use super::{InputDriver, InputHarness, accepted_snapshot};
use gpui::{TestAppContext, px};
use serde_json::{Value, json};

macro_rules! memory_band_layout_case {
    ($name:ident, $width:literal, $primary:ident, $band:ident) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Measure one installed Memory layout without changing its selected observation.
            let mut harness = InputHarness::setup_route(cx, "memory");
            harness.driver.seed_memory_band($width);
            let selected = harness.driver.accepted_selection().unwrap()["selected"].clone();

            $primary(&mut harness.driver);
            $band(&mut harness.driver);
            assert_memory_band_order(&mut harness.driver);

            assert_eq!(
                harness.driver.accepted_selection().unwrap()["selected"],
                selected
            );
            harness.teardown();
        }
    };
}

memory_band_layout_case!(
    memory_swap_paging_band_has_three_columns_at_1100,
    1100.0,
    assert_memory_primary_pair,
    assert_memory_band_three
);
memory_band_layout_case!(
    memory_swap_paging_band_has_two_columns_at_1099,
    1099.0,
    assert_memory_primary_pair,
    assert_memory_band_two
);
memory_band_layout_case!(
    memory_swap_paging_band_has_two_columns_at_840,
    840.0,
    assert_memory_primary_pair,
    assert_memory_band_two
);
memory_band_layout_case!(
    memory_swap_paging_band_has_two_columns_at_640,
    640.0,
    assert_memory_primary_pair,
    assert_memory_band_two
);
memory_band_layout_case!(
    memory_swap_paging_band_has_one_column_at_639,
    639.0,
    assert_memory_primary_stacked,
    assert_memory_band_one
);

#[gpui::test]
fn memory_swap_paging_band_regroups_across_1100_in_one_window(cx: &mut TestAppContext) {
    // Cross the breakpoint both ways while keeping the primary pair and selected identity.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_memory_band(1100.0);
    let selected = harness.driver.accepted_selection().unwrap()["selected"].clone();
    assert_memory_primary_pair(&mut harness.driver);
    assert_memory_band_three(&mut harness.driver);

    harness.driver.resize_memory_band(1099.0);

    assert_memory_primary_pair(&mut harness.driver);
    assert_memory_band_two(&mut harness.driver);

    harness.driver.resize_memory_band(1100.0);

    assert_memory_primary_pair(&mut harness.driver);
    assert_memory_band_three(&mut harness.driver);
    assert_eq!(
        harness.driver.accepted_selection().unwrap()["selected"],
        selected
    );
    harness.teardown();
}

#[gpui::test]
fn memory_swap_paging_band_hover_keeps_geometry_and_restores_the_exact_selection(
    cx: &mut TestAppContext,
) {
    // Move a real pointer across a captured second, a gap and plot exit in the three-card band.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_memory_band(1320.0);
    let bounds = harness.driver.chart_bounds("live-swap-flow-plot");
    let selected = harness.driver.accepted_selection().unwrap()["selected"].clone();

    harness
        .driver
        .hover_previous_second_on("live-swap-flow-plot");

    assert_memory_band_historical(&mut harness.driver);
    assert_eq!(harness.driver.chart_bounds("live-swap-flow-plot"), bounds);
    let gap = harness
        .driver
        .second_position("live-swap-flow-plot", 897.5 / 900.0);
    harness.driver.hover_position(gap);
    assert_memory_band_gap(&mut harness.driver);

    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-swap-flow:Unavailable")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("overview-context:memory-free:Unavailable")
    );
    assert_eq!(harness.driver.chart_bounds("live-swap-flow-plot"), bounds);
    harness
        .driver
        .leave_plot("live-swap-flow-plot", super::super::PlotExit::Sidebar);

    assert_memory_band_current(&mut harness.driver);
    assert!(
        harness
            .driver
            .reading_is_visible("overview-context:memory-free:6.0 GiB")
    );
    assert_eq!(harness.driver.chart_bounds("live-swap-flow-plot"), bounds);
    assert!(!harness.driver.cursor_is_visible("live-swap-flow-plot"));
    assert!(!harness.driver.cursor_is_visible("live-paging-plot"));
    assert_eq!(
        harness.driver.accepted_selection().unwrap()["selected"],
        selected
    );
    harness.teardown();
}

#[gpui::test]
fn memory_pressure_draws_the_prototype_timeline_beside_memory_in_use(cx: &mut TestAppContext) {
    // Goal: the Memory page's pressure is the prototype's full-height labelled timeline, and
    // its caption follows the hovered second.
    // Method: seed two captured seconds, measure the band, then hover the earlier second.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.seed_memory_band(1320.0);

    let band = harness.driver.chart_bounds("live-memory-pressure-plot");
    assert_eq!(band.size.height, px(173.0));
    assert!(
        harness
            .driver
            .reading_is_visible("chart-caption:live-memory-pressure:Warning")
    );

    harness
        .driver
        .hover_previous_second_on("live-memory-pressure-plot");

    assert!(
        harness
            .driver
            .reading_is_visible("chart-caption:live-memory-pressure:Normal")
    );
    harness.teardown();
}

impl InputDriver {
    fn seed_memory_band(&mut self, width: f32) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            for (index, point) in snapshot.selection.legacy_mut().unwrap()["trends"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .enumerate()
            {
                memory_band_point(point, index);
            }
            snapshot.selection.legacy_mut().unwrap()["summary"] =
                snapshot.selection["trends"][1].clone();
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.resize_memory_band(width);
    }

    fn resize_memory_band(&mut self, width: f32) {
        self.context
            .simulate_resize(gpui::size(px(width), px(4000.0)));
        self.context.run_until_parked();
    }
}

fn assert_memory_primary_pair(driver: &mut InputDriver) {
    let memory = driver.chart_bounds("chart-card:live-memory");
    let pressure = driver.chart_bounds("chart-card:live-memory-pressure");
    assert_eq!(memory.top(), pressure.top());
    assert_eq!(pressure.left() - memory.right(), px(8.0));
}

fn assert_memory_primary_stacked(driver: &mut InputDriver) {
    let memory = driver.chart_bounds("chart-card:live-memory");
    let pressure = driver.chart_bounds("chart-card:live-memory-pressure");
    assert_eq!(memory.left(), pressure.left());
    assert_eq!(pressure.top() - memory.bottom(), px(8.0));
}

fn assert_memory_band_three(driver: &mut InputDriver) {
    let swap = driver.chart_bounds("chart-card:live-swap");
    let traffic = driver.chart_bounds("chart-card:live-swap-flow");
    let paging = driver.chart_bounds("chart-card:live-paging");
    assert_eq!(swap.top(), traffic.top());
    assert_eq!(traffic.top(), paging.top());
    assert_eq!(traffic.left() - swap.right(), px(8.0));
    assert_eq!(paging.left() - traffic.right(), px(8.0));
}

fn assert_memory_band_two(driver: &mut InputDriver) {
    let swap = driver.chart_bounds("chart-card:live-swap");
    let traffic = driver.chart_bounds("chart-card:live-swap-flow");
    let paging = driver.chart_bounds("chart-card:live-paging");
    assert_eq!(swap.top(), traffic.top());
    assert_eq!(traffic.left() - swap.right(), px(8.0));
    assert_eq!(paging.left(), swap.left());
    assert_eq!(paging.size.width, swap.size.width);
    assert_eq!(paging.top() - swap.bottom(), px(8.0));
}

fn assert_memory_band_one(driver: &mut InputDriver) {
    let swap = driver.chart_bounds("chart-card:live-swap");
    let traffic = driver.chart_bounds("chart-card:live-swap-flow");
    let paging = driver.chart_bounds("chart-card:live-paging");
    assert_eq!(swap.left(), traffic.left());
    assert_eq!(traffic.left(), paging.left());
    assert_eq!(traffic.top() - swap.bottom(), px(8.0));
    assert_eq!(paging.top() - traffic.bottom(), px(8.0));
}

fn assert_memory_band_order(driver: &mut InputDriver) {
    let pressure = driver.chart_bounds("chart-card:live-memory-pressure");
    let heading = driver.chart_bounds("Swap and paging");
    let swap = driver.chart_bounds("chart-card:live-swap");
    let paging = driver.chart_bounds("chart-card:live-paging");
    let attribution = driver.chart_bounds("memory-attribution-unavailable");
    let quantities = driver.chart_bounds("overview-context:memory-wired:2.0 GiB");
    assert!(pressure.bottom() < heading.top());
    assert!(heading.bottom() < swap.top());
    assert!(paging.bottom() < attribution.top());
    assert!(attribution.bottom() < quantities.top());
    assert!(driver.reading_is_visible("overview-context:memory-compressed:1.0 GiB"));
    assert!(driver.reading_is_visible("overview-context:memory-pressure:Warning"));
}

fn assert_memory_band_historical(driver: &mut InputDriver) {
    assert_memory_band_reading(driver, 999, 1, "1 MB/s");
    assert!(driver.reading_is_visible("overview-context:memory-free:2.0 GiB"));
    assert_memory_band_legends(
        driver,
        [
            "chart-legend:live-paging:Page-ins:10 /s",
            "chart-legend:live-paging:Compressed:2 /s",
            "chart-legend:live-paging:Decompressed:1 /s",
        ],
    );
    assert!(driver.cursor_is_visible("live-swap-flow-plot"));
    assert!(driver.cursor_is_visible("live-paging-plot"));
}

fn assert_memory_band_gap(driver: &mut InputDriver) {
    assert_memory_band_legends(
        driver,
        [
            "chart-legend:live-paging:Page-ins:Unavailable",
            "chart-legend:live-paging:Compressed:Unavailable",
            "chart-legend:live-paging:Decompressed:Unavailable",
        ],
    );
    let (second, reading) = driver.chart_accessibility_reading("live-swap-flow");
    assert_eq!(second, Some(998));
    assert!(reading.ends_with("Unavailable"));
    driver.view.read_with(&driver.context, |dashboard, _| {
        assert!(dashboard.reading_snapshot().unwrap().selection["selected"].is_null());
    });
}

fn assert_memory_band_current(driver: &mut InputDriver) {
    assert_memory_band_reading(driver, 1000, 2, "3 MB/s");
    assert_memory_band_legends(
        driver,
        [
            "chart-legend:live-paging:Page-ins:30 /s",
            "chart-legend:live-paging:Compressed:6 /s",
            "chart-legend:live-paging:Decompressed:3 /s",
        ],
    );
}

fn assert_memory_band_legends(driver: &mut InputDriver, selectors: [&'static str; 3]) {
    for selector in selectors {
        assert!(driver.reading_is_visible(selector), "{selector}");
    }
}

fn assert_memory_band_reading(driver: &InputDriver, second: u64, publication: u64, reading: &str) {
    let (observed, accessible) = driver.chart_accessibility_reading("live-swap-flow");
    assert_eq!(observed, Some(second));
    assert!(accessible.ends_with(reading), "{accessible}");
    driver.view.read_with(&driver.context, |dashboard, _| {
        assert_eq!(
            dashboard.reading_snapshot().unwrap().selection["selected"]["publication"],
            publication
        );
    });
}

fn memory_band_point(point: &mut Value, index: usize) {
    assert!(index < 2);
    let gibibyte = 1_073_741_824_u64;
    for (key, value) in [
        ("memory_total_bytes", 16),
        ("memory_wired_bytes", 2),
        ("memory_compressed_bytes", 1),
        ("memory_inactive_bytes", 1),
        ("memory_speculative_bytes", 0),
        ("memory_anonymous_bytes", 3),
        ("memory_file_backed_bytes", 2),
        ("memory_purgeable_bytes", 1),
        ("swap_total_bytes", 4),
    ] {
        point[key] = json!(value * gibibyte);
    }
    let (active, swap, input, output, pageins, pageouts, compressed, decompressed, pressure) =
        if index == 0 {
            (2, 1, 1.0, 0.5, 10, 4, 2, 1, 0)
        } else {
            (6, 2, 3.0, 2.0, 30, 12, 6, 3, 1)
        };
    point["memory_free_bytes"] = json!(active * gibibyte);
    point["swap_used_bytes"] = json!(swap * gibibyte);
    point["swap_in_bytes_per_second"] = json!(input * 1_000_000.0);
    point["swap_out_bytes_per_second"] = json!(output * 1_000_000.0);
    point["memory_pageins_per_second"] = json!(pageins);
    point["memory_pageouts_per_second"] = json!(pageouts);
    point["memory_compressions_per_second"] = json!(compressed);
    point["memory_decompressions_per_second"] = json!(decompressed);
    point["pressure"] = json!(pressure);
}
