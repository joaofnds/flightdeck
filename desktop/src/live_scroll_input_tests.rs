use gpui::{TestAppContext, px};

use super::{InputDriver, InputHarness};

#[gpui::test]
fn inspecting_a_second_keeps_the_page_where_the_user_scrolled(cx: &mut TestAppContext) {
    // Goal: choosing a second to inspect never moves the page while its readings load.
    // Method: scroll Overview to a row, inspect a second, then deliver the reply, and read the
    // row at the top of the viewport after each step.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.scroll_rows_to("overview-analysis", px(40.0));

    let generation = harness.driver.inspect_second(999);
    let loading = harness.driver.scroll_top();
    harness.driver.deliver_reply(generation);
    let loaded = harness.driver.scroll_top();

    assert_eq!(loading, ("overview-analysis".into(), px(40.0)));
    assert_eq!(loaded, ("overview-analysis".into(), px(40.0)));
    harness.teardown();
}

#[gpui::test]
fn crossing_a_column_breakpoint_keeps_the_row_at_the_top(cx: &mut TestAppContext) {
    // Goal: resizing the window never sends the page back to its top.
    // Method: scroll Overview to a row below the charts, narrow the window until the charts
    // take one column, and read the row at the top of the viewport.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.scroll_rows_to("overview-analysis", px(0.0));

    harness.driver.resize(gpui::size(px(600.0), px(700.0)));

    assert_eq!(harness.driver.scroll_top().0, "overview-analysis");
    harness.teardown();
}

#[gpui::test]
fn hovering_a_chart_adds_no_room_past_the_last_row_when_the_page_fills_the_viewport(
    cx: &mut TestAppContext,
) {
    // Goal: the page cannot scroll past its last row while a chart's reading is shown, which
    // blank room after the last row would allow. Method: hover the first chart on Energy, whose
    // rows below fill the viewport, and read the rows the page lays out.
    let mut harness = InputHarness::setup_route(cx, "energy");
    harness.driver.scroll_rows_to("summary", px(0.0));

    harness.driver.hover_previous_second_on("live-energy-plot");

    assert!(harness.driver.has_preview());
    assert!(!harness.driver.has_room_past_the_last_row());
    harness.teardown();
}

impl InputDriver {
    fn has_preview(&self) -> bool {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.preview.is_some())
    }

    fn has_room_past_the_last_row(&self) -> bool {
        self.view.read_with(&self.context, |dashboard, _| {
            matches!(
                dashboard.rows.last(),
                Some(crate::live_dashboard::Row::Spacer(_))
            )
        })
    }

    fn scroll_rows_to(&mut self, key: &str, offset: gpui::Pixels) {
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(700.0)));
        self.context.run_until_parked();
        self.view.update(&mut self.context, |dashboard, cx| {
            let index = dashboard
                .rows
                .iter()
                .position(|row| row.layout_key() == key)
                .unwrap();
            dashboard.list.scroll_to(gpui::ListOffset {
                item_ix: index,
                offset_in_item: offset,
            });
            cx.notify();
        });
        self.context.run_until_parked();
        assert_eq!(self.scroll_top(), (key.into(), offset));
    }

    fn scroll_top(&self) -> (String, gpui::Pixels) {
        self.view.read_with(&self.context, |dashboard, _| {
            let top = dashboard.list.logical_scroll_top();
            (dashboard.rows[top.item_ix].layout_key(), top.offset_in_item)
        })
    }

    fn inspect_second(&mut self, second: u64) -> u64 {
        let generation = self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.inspect(second, cx);
            dashboard.generation
        });
        self.context.run_until_parked();
        generation
    }

    fn deliver_reply(&mut self, generation: u64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            assert!(dashboard.receive(crate::live_source::Update::Ready(
                super::accepted_snapshot(generation)
            )));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn resize(&mut self, size: gpui::Size<gpui::Pixels>) {
        self.context.simulate_resize(size);
        self.context.run_until_parked();
    }
}
