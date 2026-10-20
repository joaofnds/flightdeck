use gpui::{Modifiers, MouseButton, TestAppContext, px};

use super::{InputDriver, InputHarness, accepted_snapshot};

#[path = "live_section_regression_tests.rs"]
mod regressions;

macro_rules! disclosure_case {
    ($name:ident, $route:literal, $section:literal, [$($content:literal),+], $outside:literal) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Goal: preserve prototype boundaries. Method: click each installed disclosure twice.
            let mut harness = InputHarness::setup_route(cx, $route);
            harness.driver.prepare_sections();
            assert!(harness.driver.reading_is_visible($section));
            $(assert!(harness.driver.reading_is_visible($content));)+
            assert!(harness.driver.reading_is_visible($outside));
            let generation = harness.driver.section_generation();

            harness.driver.click_section($section);

            $(assert!(!harness.driver.reading_is_visible($content));)+
            assert!(harness.driver.reading_is_visible($outside));
            assert_eq!(harness.driver.section_generation(), generation);
            harness.driver.click_section($section);
            $(assert!(harness.driver.reading_is_visible($content));)+
            harness.teardown();
        }
    };
}

disclosure_case!(
    overview_histories_disclosure_keeps_context_outside,
    "overview",
    "section:overview-histories",
    ["overview-app-history-cards"],
    "overview-context:cooling-battery"
);
disclosure_case!(
    overview_context_disclosure_keeps_primary_pressure_outside,
    "overview",
    "section:overview-context",
    ["live-correlation-plot"],
    "live-memory-pressure-plot"
);
disclosure_case!(
    app_totals_disclosure_keeps_executable_groups_outside,
    "apps",
    "section:app-totals",
    [
        "live-app-cpu-plot",
        "live-app-resident-plot",
        "live-app-gpu-plot",
        "live-app-power-plot",
        "live-app-disk-plot"
    ],
    "app-column:executables:Disk I/O"
);
disclosure_case!(
    memory_quantities_disclosure_keeps_composition_outside,
    "memory",
    "section:memory-quantities",
    [
        "overview-context:memory-wired:Unavailable",
        "overview-context:memory-cached:Unavailable"
    ],
    "live-memory-plot"
);
disclosure_case!(
    computer_reference_disclosure_keeps_model_outside,
    "computer",
    "section:computer-reference",
    [
        "computer-airflow-stop:50 °C",
        "computer-reference-paragraph:1"
    ],
    "computer-specifications"
);

#[gpui::test]
fn overview_disclosures_keep_independent_folds_through_updates_and_hover(cx: &mut TestAppContext) {
    // Goal: redraws retain independent folds. Method: accept updates and hover outside the fold.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.prepare_sections();
    harness.driver.click_section("section:overview-histories");

    harness.driver.advance_section_reading();
    harness.driver.hover_previous_second_on("live-cpu-plot");

    assert!(
        !harness
            .driver
            .reading_is_visible("overview-app-history-cards")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("overview-context:cooling-battery")
    );
    assert_eq!(
        harness.driver.chart_accessibility_reading("live-cpu").0,
        Some(999)
    );
    assert_eq!(
        harness.driver.accepted_selection().unwrap()["selected"]["publication"],
        3
    );
    harness
        .driver
        .leave_plot("live-cpu-plot", super::PlotExit::Sidebar);
    harness.driver.click_section("section:overview-context");
    assert!(
        !harness
            .driver
            .reading_is_visible("overview-app-history-cards")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("overview-context:cooling-battery")
    );
    harness.driver.click_section("section:overview-histories");
    assert!(
        harness
            .driver
            .reading_is_visible("overview-app-history-cards")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("overview-context:cooling-battery")
    );
    harness.teardown();
}

#[gpui::test]
fn overview_primary_context_precedes_rankings_and_survives_supplemental_folds(
    cx: &mut TestAppContext,
) {
    // v30 keeps primary pressure, thermal and cooling context above rankings and outside folds.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.prepare_sections();
    let selected = harness.driver.accepted_selection().unwrap()["selected"].clone();

    harness.driver.click_section("section:overview-histories");
    harness.driver.click_section("section:overview-context");

    for selector in [
        "live-memory-pressure-plot",
        "overview-context:thermal",
        "overview-context:cooling-battery",
    ] {
        let bounds = harness.driver.chart_bounds(selector);
        let rankings = harness.driver.chart_bounds("Top apps at the selected time");
        assert!(
            bounds.bottom() < rankings.top(),
            "{selector} must precede rankings"
        );
    }
    assert_eq!(
        harness.driver.accepted_selection().unwrap()["selected"],
        selected
    );
    harness.teardown();
}

#[gpui::test]
fn disclosure_navigation_resets_folds_only_when_the_route_changes(cx: &mut TestAppContext) {
    // Goal: match v30 navigation lifetime. Method: revisit the same route, then leave and return.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.prepare_sections();
    harness.driver.click_section("section:overview-histories");

    harness.driver.click_section("navigation:overview");
    harness.driver.prepare_sections();

    assert!(
        !harness
            .driver
            .reading_is_visible("overview-app-history-cards")
    );
    harness.driver.click_section("navigation:cpu");
    harness.driver.click_section("navigation:overview");
    harness.driver.prepare_sections();
    assert!(
        harness
            .driver
            .reading_is_visible("overview-app-history-cards")
    );
    harness.teardown();
}

#[gpui::test]
fn disclosure_keeps_keyboard_focus_and_toggles_with_enter_and_space(cx: &mut TestAppContext) {
    // Goal: operate the disclosure without a pointer. Method: retain click focus across key input.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.prepare_sections();
    harness.driver.click_section("section:energy-context");
    assert!(
        !harness
            .driver
            .reading_is_visible("live-soc-temperature-plot")
    );
    let generation = harness.driver.section_generation();
    harness.driver.prepare_sections();

    harness.driver.press("enter");

    assert!(
        harness
            .driver
            .reading_is_visible("live-soc-temperature-plot")
    );
    harness.driver.press("space");
    assert!(
        !harness
            .driver
            .reading_is_visible("live-soc-temperature-plot")
    );
    harness.driver.press("left");
    assert!(
        !harness
            .driver
            .reading_is_visible("live-soc-temperature-plot")
    );
    assert_eq!(harness.driver.section_generation(), generation);
    harness.teardown();
}

#[gpui::test]
fn disclosure_folding_keeps_the_scrolled_header_in_place(cx: &mut TestAppContext) {
    // Goal: folding does not jump to the top. Method: fold a disclosure in a scrolled viewport.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.prepare_sections();
    harness.driver.scroll_to_section();
    let before = harness.driver.chart_bounds("section:overview-histories");

    harness.driver.click_section("section:overview-histories");

    assert_eq!(
        harness
            .driver
            .chart_bounds("section:overview-histories")
            .top(),
        before.top()
    );
    assert!(
        harness
            .driver
            .reading_is_visible("section:overview-context")
    );
    harness.teardown();
}

#[gpui::test]
fn memory_disclosure_keeps_the_hovered_observation_when_reopened_from_the_keyboard(
    cx: &mut TestAppContext,
) {
    // Goal: a local fold retains inspected data. Method: reopen while another plot owns hover.
    let mut harness = InputHarness::setup_route(cx, "memory");
    harness.driver.prepare_sections();
    harness.driver.click_section("section:memory-quantities");
    harness.driver.hover_previous_second_on("live-memory-plot");
    let generation = harness.driver.section_generation();

    harness.driver.press("enter");

    assert!(
        harness
            .driver
            .reading_is_visible("overview-context:memory-free:2.0 GiB")
    );
    assert_eq!(
        harness.driver.chart_accessibility_reading("live-memory").0,
        Some(999)
    );
    assert_eq!(harness.driver.section_generation(), generation);
    harness.teardown();
}

#[gpui::test]
fn energy_disclosure_hides_thermal_context_and_keeps_cooling_visible(cx: &mut TestAppContext) {
    // Goal: fold only prototype details. Method: click the rendered Energy disclosure.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.prepare_sections();
    assert!(
        harness
            .driver
            .reading_is_visible("live-soc-temperature-plot")
    );
    assert!(harness.driver.reading_is_visible("live-fans-plot"));
    assert!(
        harness
            .driver
            .reading_is_visible("live-power-breakdown-plot")
    );
    assert!(harness.driver.reading_is_visible("section:energy-context"));
    let selection = harness.driver.accepted_selection();
    let generation = harness.driver.section_generation();

    harness.driver.click_section("section:energy-context");

    assert!(
        !harness
            .driver
            .reading_is_visible("live-soc-temperature-plot")
    );
    assert!(harness.driver.reading_is_visible("live-fans-plot"));
    assert!(
        !harness
            .driver
            .reading_is_visible("live-power-breakdown-plot")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("Cooling and temperatures")
    );
    assert_eq!(harness.driver.accepted_selection(), selection);
    assert_eq!(harness.driver.section_generation(), generation);
    harness.teardown();
}

impl InputDriver {
    fn seed_energy_breakdown(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.selection["trends"][1]["power_watts"] = serde_json::json!(15);
            snapshot.selection["trends"][1]["gpu_watts"] = serde_json::json!(5);
            snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn assert_energy_breakdown(&mut self, expected: &BreakdownReading) {
        let (second, _) = self.chart_accessibility_reading("live-power-breakdown");
        assert_eq!(second, Some(expected.second));
        assert!(self.reading_is_visible(expected.headline));
        assert!(self.reading_is_visible(expected.remainder_legend));
        assert!(self.reading_is_visible(expected.gpu_legend));
    }

    fn advance_section_reading(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.selection["selected"]["publication"] = serde_json::json!(3);
            snapshot.selection["trends"][1]["publication"] = serde_json::json!(3);
            snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
            dashboard.accept(snapshot);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn scroll_to_section(&mut self) {
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(700.0)));
        self.context.run_until_parked();
        self.view.update(&mut self.context, |dashboard, cx| {
            let index = dashboard
                .rows
                .iter()
                .position(|row| {
                    matches!(
                        row,
                        crate::live_dashboard::Row::Disclosure(
                            crate::live_sections::Section::OverviewHistories
                        )
                    )
                })
                .unwrap();
            dashboard.list.scroll_to(gpui::ListOffset {
                item_ix: index,
                offset_in_item: px(0.0),
            });
            cx.notify();
        });
        self.context.run_until_parked();
    }

    pub(super) fn prepare_sections(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            for (index, point) in snapshot.selection["trends"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .enumerate()
            {
                point["fan_rpm"] = serde_json::json!([1500, 1700]);
                point["soc_celsius"] = serde_json::json!(42);
                if dashboard.request.route == "memory" {
                    point["memory_free_bytes"] =
                        serde_json::json!([2_147_483_648_u64, 4_294_967_296][index]);
                }
            }
            snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
            if dashboard.request.route == "apps" {
                let points: Vec<_> = snapshot.selection["trends"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|point| {
                        serde_json::json!({"observation":{
                        "stamp":point,"wall_ns":point["wall_ns"]},"segment":0})
                    })
                    .collect();
                snapshot.history = Some(serde_json::json!({
                    "selected":snapshot.selection["selected"],
                    "points":points
                }));
            }
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(6500.0)));
        self.context.run_until_parked();
    }

    fn click_section(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).unwrap();
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::none());
        self.context.run_until_parked();
    }

    fn section_generation(&self) -> u64 {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.generation)
    }
}

struct BreakdownReading {
    second: u64,
    headline: &'static str,
    remainder_legend: &'static str,
    gpu_legend: &'static str,
}

#[gpui::test]
fn energy_breakdown_inspects_both_components_and_total_then_clears_gaps_and_restores(
    cx: &mut TestAppContext,
) {
    // Inspect real rendered readouts so neither the total nor a component can retain live data.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.prepare_sections();
    harness.driver.seed_energy_breakdown();
    harness.driver.assert_energy_breakdown(&BreakdownReading {
        second: 1000,
        headline: "chart-reading:live-power-breakdown:15 W",
        remainder_legend: "chart-legend:live-power-breakdown:SoC and rest of system:10 W",
        gpu_legend: "chart-legend:live-power-breakdown:GPU:5 W",
    });

    harness
        .driver
        .hover_previous_second_on("live-power-breakdown-plot");

    harness.driver.assert_energy_breakdown(&BreakdownReading {
        second: 999,
        headline: "chart-reading:live-power-breakdown:8 W",
        remainder_legend: "chart-legend:live-power-breakdown:SoC and rest of system:6 W",
        gpu_legend: "chart-legend:live-power-breakdown:GPU:2 W",
    });
    let gap = harness
        .driver
        .second_position("live-power-breakdown-plot", 897.5 / 900.0);
    harness.driver.hover_position(gap);
    harness.driver.assert_energy_breakdown(&BreakdownReading {
        second: 998,
        headline: "chart-reading:live-power-breakdown:Unavailable",
        remainder_legend: "chart-legend:live-power-breakdown:SoC and rest of system:Unavailable",
        gpu_legend: "chart-legend:live-power-breakdown:GPU:Unavailable",
    });
    harness
        .driver
        .leave_plot("live-power-breakdown-plot", super::PlotExit::Sidebar);
    harness.driver.assert_energy_breakdown(&BreakdownReading {
        second: 1000,
        headline: "chart-reading:live-power-breakdown:15 W",
        remainder_legend: "chart-legend:live-power-breakdown:SoC and rest of system:10 W",
        gpu_legend: "chart-legend:live-power-breakdown:GPU:5 W",
    });
    harness.teardown();
}
