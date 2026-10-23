use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::json;
use viewer::store::{MachineSecond, View};

use super::super::LiveDashboard;
use crate::data::Panel;
use crate::live_hover::{Origin, Preview, Target};
use crate::live_projection::overview_panels;
use crate::live_source::{Snapshot, Worker};
use crate::live_store_fixtures::{
    levels, levels_named, reading_at, selection, store_levels_named, trend_point, view,
};
use crate::live_views::summary;

#[test]
fn a_view_newer_than_the_readings_draws_the_overviews_machine_charts() {
    // Goal: Overview, following the newest second, draws its machine charts from the store's
    // View, which holds seconds the worker has not read yet. Method: readings that end at
    // 10,000, then a View that ends five seconds later.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    let view = newer_view();

    let changed = dashboard.take_view(Arc::clone(&view));

    assert!(changed);
    for panel in overview_panels(&view, levels().as_ref()).unwrap() {
        assert_eq!(drawn(&dashboard, &panel.id).lines, panel.lines);
    }
}

#[test]
fn the_range_ends_at_the_views_newest_second() {
    // Goal: the charts drawn from the View show its newest second, which the worker's readings
    // do not hold yet. Method: a View five seconds newer than the readings.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);

    dashboard.take_view(newer_view());

    let window = dashboard.view_window().unwrap();
    assert_eq!(window.first + window.span - 1, 10_005);
    assert_eq!(drawn(&dashboard, "live-cpu").window, Some(window));
}

#[test]
fn a_view_newer_than_the_readings_fills_the_overviews_readouts() {
    // Goal: Overview's readouts show the newest second the store holds, as its charts do.
    // Method: readings that end at 10,000, then a View that ends at 10,005, whose readouts read
    // as the worker's reading of 10,005 reads them.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);

    dashboard.take_view(newer_view());

    let expected = summary::cards("overview", &summary_selection(&reading_at(10_005, 0)));
    assert_eq!(texts(&dashboard.summary_cards()), texts(&expected));
}

#[test]
fn the_cpu_chart_names_the_levels_of_the_views_newest_second() {
    // Goal: the View's own seconds name the CPU chart's levels, so the chart takes nothing from
    // the worker's readings. Method: a View whose newest second names performance and
    // efficiency cores, after readings that name super and performance cores.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    let mut newest = reading_at(10_005, 0);
    newest.performance_levels = Some(store_levels_named("Performance", "Efficiency"));
    let view = Arc::new(view(1, &[], &[reading_at(10_000, 0), newest]));

    dashboard.take_view(Arc::clone(&view));

    let levels = levels_named("Performance", "Efficiency");
    let expected = overview_panels(&view, levels.as_ref()).unwrap();
    let cpu = expected
        .iter()
        .find(|panel| panel.id == "live-cpu")
        .unwrap();
    assert_eq!(drawn(&dashboard, "live-cpu").lines, cpu.lines);
}

#[test]
fn a_reading_that_arrives_after_the_view_redraws_no_machine_chart() {
    // Goal: the worker's next reading leaves the charts the View drew as they are, so their
    // plots draw nothing again. Method: take a View, then accept a reading older than it.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    let view = newer_view();
    dashboard.take_view(Arc::clone(&view));
    let before = machine_panels(&dashboard, &view);

    dashboard.accept(readings(&[reading_at(10_000, 0), reading_at(10_001, 0)]));

    let after = machine_panels(&dashboard, &view);
    assert_eq!(after.len(), before.len());
    for (after, before) in after.iter().zip(&before) {
        assert!(Rc::ptr_eq(after, before));
    }
}

mod when_the_page_shows_a_chosen_second {
    use super::*;

    #[test]
    fn the_worker_s_readings_draw_the_charts() {
        // Goal: a chosen second keeps the charts of its own readings, which the worker read
        // around it. Method: choose a second, then take a newer View.
        let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        dashboard.request.second = Some(10_000);
        let before = dashboard.panels.clone();

        let changed = dashboard.take_view(newer_view());

        assert!(!changed);
        assert_same_panels(&dashboard.panels, &before);
    }
}

mod when_the_page_is_another_page {
    use super::*;

    #[test]
    fn the_worker_s_readings_draw_the_charts() {
        // Goal: only Overview draws from the View yet, so every other page keeps the charts of
        // its own readings. Method: the CPU page, then a newer View.
        let mut dashboard = dashboard_on("cpu", &[reading_at(9_999, 0), reading_at(10_000, 0)]);
        let before = dashboard.panels.clone();

        let changed = dashboard.take_view(newer_view());

        assert!(!changed);
        assert_same_panels(&dashboard.panels, &before);
    }
}

mod when_the_view_is_older_than_the_readings {
    use super::*;

    #[test]
    fn the_worker_s_readings_draw_the_charts() {
        // Goal: a store that falls behind the worker never draws less than the worker read.
        // Method: readings that end at 10,000, then a View that ends a second before.
        let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        let before = dashboard.panels.clone();

        let changed = dashboard.take_view(Arc::new(view(1, &[], &[reading_at(9_999, 0)])));

        assert!(!changed);
        assert_same_panels(&dashboard.panels, &before);
    }
}

mod when_a_hover_shows_an_earlier_second {
    use super::*;

    #[test]
    fn the_charts_stay_where_the_pointer_found_them() {
        // Goal: a hover holds the page still, as it holds the worker's readings, so the second
        // under the pointer stays under it. Method: show a hover's reading, then take a newer
        // View.
        let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        dashboard.preview = Some(preview(&dashboard));
        let before = dashboard.panels.clone();

        let changed = dashboard.take_view(newer_view());

        assert!(!changed);
        assert_same_panels(&dashboard.panels, &before);
    }

    #[test]
    fn the_readouts_show_the_hovered_second() {
        // Goal: the readouts read the second under the pointer, which the worker read for the
        // hover, rather than the View's newest. Method: show a hover's reading of 9,999, then
        // take a View that ends at 10,005.
        let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        let mut hover = preview(&dashboard);
        hover.reading.selection = summary_selection(&reading_at(9_999, 0));
        dashboard.preview = Some(hover);

        dashboard.take_view(newer_view());

        let expected = summary::cards("overview", &summary_selection(&reading_at(9_999, 0)));
        assert_eq!(texts(&dashboard.summary_cards()), texts(&expected));
    }
}

mod when_macos_hides_the_window {
    use super::*;

    #[test]
    fn the_window_lets_go_of_the_view() {
        // Goal: a hidden window holds no View, so the store writes its buffers again. Method:
        // take a View, then hide the window.
        let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        let view = newer_view();
        dashboard.take_view(Arc::clone(&view));

        dashboard.tick_hidden();

        assert_eq!(Arc::strong_count(&view), 1);
    }
}

/// An Overview that shows the worker's readings of `trends`, following the newest second.
fn dashboard(trends: &[MachineSecond]) -> LiveDashboard {
    dashboard_on("overview", trends)
}

/// The page `route` showing the worker's readings of `trends`, following the newest second.
fn dashboard_on(route: &str, trends: &[MachineSecond]) -> LiveDashboard {
    let root = std::env::temp_dir().join(format!(
        "flightdeck-missing-store-view-{}",
        std::process::id()
    ));
    let mut dashboard = LiveDashboard::new(Worker::start(root).unwrap(), route.into(), None);
    dashboard.accept(readings(trends));
    assert!(dashboard.error.is_none());
    dashboard
}

/// The View of 9,999 to 10,005, five seconds newer than the worker's readings.
fn newer_view() -> Arc<View> {
    let seconds = [
        reading_at(9_999, 0),
        reading_at(10_000, 0),
        reading_at(10_005, 0),
    ];
    Arc::new(view(1, &[], &seconds))
}

/// The worker's readings of `trends`, following the newest of them.
fn readings(trends: &[MachineSecond]) -> Box<Snapshot> {
    let mut selection = selection(&[], trends);
    selection["summary"] = json!({"memory_total_bytes": 34_359_738_368_u64});

    Box::new(Snapshot {
        generation: 0,
        catalog: json!({"first_second": 9_000, "last_second": 10_000}),
        selection,
        pages: BTreeMap::from([(
            "ticks".into(),
            json!({"offset":0,"total":1,"rows":[["1"]],"columns":["publication"]}),
        )]),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: None,
        cpu_history: None,
        resident_history: None,
        app_attribution_history: None,
        overview_app_histories: None,
        histories_pending: false,
        process_history: None,
        rankings: None,
        app_ranks: None,
        app_parts: None,
        read_ms: 0.0,
    })
}

/// A hover's reading of 9,999, complete.
fn preview(dashboard: &LiveDashboard) -> Preview {
    let identity = json!({"run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004", "publication": 9_999});

    Preview {
        source: "".into(),
        origin: Origin::Pointer,
        target: Target::exact(9_999, &identity).unwrap(),
        reading: readings(&[reading_at(9_999, 0)]),
        base_request: dashboard.request.clone(),
        request: dashboard.request.clone(),
        generation: None,
        complete: true,
        failure: None,
        executables: Vec::new(),
        layout: None,
        header_height: None,
        retarget_geometry: false,
        page_reply_kept: false,
        hovered_at: None,
    }
}

fn drawn<'a>(dashboard: &'a LiveDashboard, id: &str) -> &'a Panel {
    dashboard
        .panels
        .iter()
        .find(|panel| panel.id == id)
        .unwrap()
}

/// The page's charts of the machine charts `view` draws, in their order.
fn machine_panels(dashboard: &LiveDashboard, view: &View) -> Vec<Rc<Panel>> {
    let expected = overview_panels(view, levels().as_ref()).unwrap();
    let mut panels = Vec::with_capacity(expected.len());
    for panel in &expected {
        let found = dashboard.panels.iter().find(|drawn| drawn.id == panel.id);
        panels.push(Rc::clone(found.unwrap()));
    }
    panels
}

/// What each of `cards` shows, in order.
fn texts(cards: &[summary::Card]) -> Vec<(String, String, String, u32)> {
    cards
        .iter()
        .map(|card| {
            let label = card.label.clone();
            (label, card.reading.clone(), card.note.clone(), card.color)
        })
        .collect()
}

/// The selection of `second` alone, which the worker's readings of that second summarize.
fn summary_selection(second: &MachineSecond) -> serde_json::Value {
    let mut selection = selection(&[], std::slice::from_ref(second));
    selection["summary"] = trend_point(second);
    selection
}

fn assert_same_panels(panels: &[Rc<Panel>], expected: &[Rc<Panel>]) {
    assert_eq!(panels.len(), expected.len());
    for (panel, expected) in panels.iter().zip(expected) {
        assert!(Rc::ptr_eq(panel, expected));
    }
}
