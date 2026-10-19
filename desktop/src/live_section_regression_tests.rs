use gpui::{TestAppContext, px};

use super::{InputDriver, InputHarness};

#[gpui::test]
fn collapsing_the_hover_owner_restores_committed_readings_with_existing_leave_behavior(
    cx: &mut TestAppContext,
) {
    // Goal: remove orphaned hover. Method: fold its owner before any exact-reading submission.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.prepare_sections();
    harness.driver.seed_energy_breakdown();
    harness.driver.focus_expanded_energy_section();
    harness
        .driver
        .hover_previous_second_on("live-power-breakdown-plot");
    let generation = harness.driver.section_generation();
    let request = harness.driver.section_request();
    assert!(harness.driver.has_section_preview());

    harness.driver.press("space");

    assert!(!harness.driver.has_section_preview());
    assert!(!harness.driver.has_section_chart_hover());
    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-energy:15 W")
    );
    assert!(harness.driver.section_generation() > generation);
    assert_eq!(harness.driver.section_request(), request);
    harness.driver.move_section_pointer_to_sidebar();
    assert!(!harness.driver.has_section_preview());
    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-energy:15 W")
    );
    harness.teardown();
}

#[gpui::test]
fn collapsing_a_submitted_hover_restores_the_base_request_and_ignores_its_late_reply(
    cx: &mut TestAppContext,
) {
    // Goal: retire a hidden hover request. Method: submit exact details, fold, then deliver late.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.prepare_sections();
    harness.driver.seed_energy_breakdown();
    harness.driver.focus_expanded_energy_section();
    harness
        .driver
        .hover_previous_second_on("live-power-breakdown-plot");
    let retired = harness.driver.submit_section_preview();
    let generation = harness.driver.section_generation();

    harness.driver.press("space");

    assert!(!harness.driver.has_section_preview());
    assert!(!harness.driver.has_section_chart_hover());
    assert!(harness.driver.section_generation() > generation);
    harness.driver.deliver_retired_section_preview(retired);
    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-energy:15 W")
    );
    harness.driver.move_section_pointer_to_sidebar();
    assert!(!harness.driver.has_section_preview());
    harness.teardown();
}

#[gpui::test]
fn collapsing_a_superseded_hover_still_restores_the_previously_redirected_worker(
    cx: &mut TestAppContext,
) {
    // Goal: preserve restoration across targets. Method: supersede a submitted hover before fold.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.prepare_sections();
    harness.driver.seed_energy_breakdown();
    harness.driver.focus_expanded_energy_section();
    harness
        .driver
        .hover_previous_second_on("live-power-breakdown-plot");
    harness.driver.submit_section_preview();
    harness
        .driver
        .hover_selected_second_on("live-power-breakdown-plot");
    let generation = harness.driver.section_generation();

    harness.driver.press("space");

    assert!(!harness.driver.has_section_preview());
    assert!(harness.driver.section_generation() > generation);
    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-energy:15 W")
    );
    harness.teardown();
}

macro_rules! offscreen_disclosure_case {
    ($name:ident, $route:literal, $section:literal, $content:literal, $refresh:ident) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Goal: retain a virtualized key handler. Method: focus, scroll away, refresh, type.
            let mut harness = InputHarness::setup_route(cx, $route);
            harness.driver.prepare_sections();
            harness.driver.click_section($section);
            harness.driver.click_section($section);
            assert!(harness.driver.reading_is_visible($content));
            harness.driver.scroll_disclosure_out_of_view();
            harness.driver.$refresh();

            harness.driver.press("space");
            harness.driver.press("enter");
            harness.driver.press("space");
            harness.driver.return_to_disclosure();

            assert!(!harness.driver.reading_is_visible($content));
            harness.driver.press("enter");
            assert!(harness.driver.reading_is_visible($content));
            harness.teardown();
        }
    };
}

offscreen_disclosure_case!(
    standalone_disclosure_keeps_its_key_handler_outside_overdraw,
    "energy",
    "section:energy-context",
    "live-power-breakdown-plot",
    redraw_section_frame
);
offscreen_disclosure_case!(
    standalone_disclosure_keeps_its_key_handler_after_an_offscreen_live_update,
    "energy",
    "section:energy-context",
    "live-power-breakdown-plot",
    advance_section_reading
);
offscreen_disclosure_case!(
    nested_disclosure_keeps_its_key_handler_outside_overdraw,
    "memory",
    "section:memory-quantities",
    "overview-context:memory-wired:Unavailable",
    redraw_section_frame
);
offscreen_disclosure_case!(
    nested_disclosure_keeps_its_key_handler_after_an_offscreen_live_update,
    "memory",
    "section:memory-quantities",
    "overview-context:memory-wired:Unavailable",
    advance_section_reading
);

impl InputDriver {
    fn section_request(&self) -> crate::live_source::Request {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.request.clone())
    }

    fn focus_expanded_energy_section(&mut self) {
        self.click_section("section:energy-context");
        self.click_section("section:energy-context");
    }

    fn has_section_preview(&self) -> bool {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.preview.is_some())
    }

    fn has_section_chart_hover(&self) -> bool {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.chart_hover.is_some()
        })
    }

    fn submit_section_preview(&mut self) -> u64 {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick_at(std::time::Instant::now(), cx);
            dashboard.preview.as_ref().unwrap().generation.unwrap()
        })
    }

    fn deliver_retired_section_preview(&mut self, generation: u64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            assert!(!dashboard.receive(crate::live_source::Update::Ready(
                super::accepted_snapshot(generation)
            )));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn move_section_pointer_to_sidebar(&mut self) {
        self.hover_position(gpui::point(px(20.0), px(300.0)));
        self.context.run_until_parked();
    }

    fn scroll_disclosure_out_of_view(&mut self) {
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(350.0)));
        self.context.run_until_parked();
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.list.scroll_to(gpui::ListOffset {
                item_ix: 0,
                offset_in_item: px(0.0),
            });
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn return_to_disclosure(&mut self) {
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(6500.0)));
        self.context.run_until_parked();
    }

    fn redraw_section_frame(&mut self) {
        self.view.update(&mut self.context, |_, cx| cx.notify());
        self.context.run_until_parked();
    }
}
