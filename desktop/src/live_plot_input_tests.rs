use gpui::{TestAppContext, px};

use super::{InputDriver, InputHarness, accepted_snapshot};
use crate::live_source::Update;

#[gpui::test]
fn a_redraw_with_nothing_new_draws_no_chart_again(cx: &mut TestAppContext) {
    // Goal: drawing the page again builds no chart's plot again when nothing a chart shows
    // changed. Method: draw the whole Overview, notify the page with nothing changed, and count
    // the page's renders and the plots' renders.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.show_the_whole_page();
    let drawn = harness.driver.draws();
    assert!(drawn.plots > 0);

    harness.driver.redraw_the_page();

    assert!(harness.driver.draws().page > drawn.page);
    assert_eq!(harness.driver.draws().plots, drawn.plots);
    harness.teardown();
}

#[gpui::test]
fn hovering_a_line_chart_draws_no_chart_again(cx: &mut TestAppContext) {
    // Goal: a hover draws the cursor and the readouts over the plots already drawn, since a
    // line chart draws the same readings whatever second is selected. Method: draw the Energy
    // page, move the pointer onto an earlier second of its chart, and count the plots' renders.
    let mut harness = InputHarness::setup(cx);
    let drawn = harness.driver.draws();

    harness.driver.hover_previous_second();

    assert!(harness.driver.draws().page > drawn.page);
    assert_eq!(harness.driver.draws().plots, drawn.plots);
    harness.teardown();
}

#[gpui::test]
fn a_later_reading_draws_the_charts_again(cx: &mut TestAppContext) {
    // Goal: a held plot never shows older readings than the page holds. Method: draw the
    // Energy page, accept a snapshot whose selected second reads another power, and count the
    // plots' renders.
    let mut harness = InputHarness::setup(cx);
    let drawn = harness.driver.draws();

    harness.driver.accept_a_later_reading();

    assert!(harness.driver.draws().plots > drawn.plots);
    harness.teardown();
}

#[gpui::test]
fn hiding_a_series_draws_its_chart_again(cx: &mut TestAppContext) {
    // Goal: a held plot stops drawing a series the moment its legend hides it. Method: draw
    // the Energy page, hide the energy chart's first series as its legend does, and count the
    // plots' renders.
    let mut harness = InputHarness::setup(cx);
    let drawn = harness.driver.draws();

    harness.driver.hide_the_first_energy_series();

    assert!(harness.driver.draws().plots > drawn.plots);
    harness.teardown();
}

#[gpui::test]
fn a_wider_window_draws_the_charts_again(cx: &mut TestAppContext) {
    // Goal: a held plot is drawn again at its new size when its card changes width. Method:
    // draw the Energy page, widen the window, and count the plots' renders.
    let mut harness = InputHarness::setup(cx);
    let drawn = harness.driver.draws();

    harness.driver.widen_the_window();

    assert!(harness.driver.draws().plots > drawn.plots);
    harness.teardown();
}

#[gpui::test]
fn leaving_a_page_lets_go_of_its_plots(cx: &mut TestAppContext) {
    // Goal: the page holds the readings of the charts it draws and no others, since each held
    // plot keeps its panel. Method: draw the whole Overview, go to the Energy page and give it
    // its readings, and count the plots the page holds against the charts Energy draws.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.show_the_whole_page();
    let overview = harness.driver.plots_held();

    harness.driver.go_to("energy");
    harness.driver.seed_accepted_selection();
    let drawn = harness.driver.charts_drawn();

    assert!(drawn > 0);
    assert!(harness.driver.plots_held() < overview);
    assert_eq!(harness.driver.plots_held(), drawn);
    harness.teardown();
}

/// How many times the page and its charts' plots have rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Draws {
    page: u64,
    plots: u64,
}

impl InputDriver {
    fn draws(&self) -> Draws {
        self.view.read_with(&self.context, |dashboard, _| Draws {
            page: dashboard.renders,
            plots: dashboard.plots.renders(),
        })
    }

    fn plots_held(&self) -> usize {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.plots.held())
    }

    /// How many of the page's chart cards the last frame drew.
    fn charts_drawn(&mut self) -> usize {
        let ids = self.view.read_with(&self.context, |dashboard, _| {
            let ids: Vec<String> = dashboard
                .panels
                .iter()
                .map(|panel| panel.id.clone())
                .collect();
            ids
        });
        ids.into_iter()
            .filter(|id| {
                let selector: &'static str = Box::leak(format!("chart-card:{id}").into_boxed_str());
                self.context.debug_bounds(selector).is_some()
            })
            .count()
    }

    fn go_to(&mut self, route: &str) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.navigate(route, cx);
        });
        self.context.run_until_parked();
    }

    /// Notifies the page with nothing changed, so it draws again from what it holds.
    fn redraw_the_page(&mut self) {
        self.view.update(&mut self.context, |_, cx| cx.notify());
        self.context.run_until_parked();
    }

    fn show_the_whole_page(&mut self) {
        self.context
            .simulate_resize(gpui::size(px(1536.0), px(5000.0)));
        self.context.run_until_parked();
    }

    fn widen_the_window(&mut self) {
        let size = self.context.update(|window, _| window.viewport_size());
        self.context
            .simulate_resize(gpui::size(size.width + px(200.0), size.height));
        self.context.run_until_parked();
    }

    fn accept_a_later_reading(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.selection["trends"][1]["power_watts"] = serde_json::json!(31.0);
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn hide_the_first_energy_series(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let panel = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-energy")
                .map(std::rc::Rc::clone)
                .expect("the Energy page has an energy chart");
            let key = dashboard.chart_legend_key(&panel, 0);
            dashboard.toggle_chart_legend(&key, cx);
        });
        self.context.run_until_parked();
    }
}
