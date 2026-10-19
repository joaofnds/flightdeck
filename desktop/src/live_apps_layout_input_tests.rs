use super::*;
use gpui::{Bounds, Pixels, Point, px};

struct AppsGridBounds {
    named: [Bounds<Pixels>; 8],
    app: [Bounds<Pixels>; 5],
    executable: Option<Bounds<Pixels>>,
    totals: Bounds<Pixels>,
    instances: Bounds<Pixels>,
}

struct AppsGridReading {
    accepted_publication: Option<u64>,
    accepted_identity: Value,
    reading_second: Option<u64>,
    reading_publication: Option<u64>,
    reading_identity: Value,
    app: Option<AppKey>,
    executable: Option<String>,
    preview_source: Option<String>,
    preview_load_history: Option<bool>,
    context: Option<std::sync::Arc<ProcessHistory>>,
    chart_readings: [String; 4],
}

struct OverviewGridBounds {
    primary_first: Bounds<Pixels>,
    primary_last: Bounds<Pixels>,
    pressure: Bounds<Pixels>,
    temperature: Bounds<Pixels>,
}

macro_rules! apps_grid_width_case {
    ($name:ident, $width:literal, $columns:literal) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Read the installed eight-plus-five card layout at one literal media breakpoint.
            let mut harness = AppHoverHarness::setup(cx);
            harness.driver.prepare_apps_grid($width);

            let bounds = harness.driver.apps_grid_bounds();

            expect_apps_grid(&bounds, $columns);
            assert_eq!(
                harness.driver.apps_grid_reading().accepted_publication,
                Some(2)
            );
            harness.teardown();
        }
    };
}

apps_grid_width_case!(apps_grid_keeps_three_named_tracks_at_1100, 1100.0, 3);
apps_grid_width_case!(apps_grid_keeps_two_named_tracks_at_1099, 1099.0, 2);
apps_grid_width_case!(apps_grid_keeps_two_named_tracks_at_640, 640.0, 2);
apps_grid_width_case!(apps_grid_keeps_one_named_track_at_639, 639.0, 1);

#[gpui::test]
fn apps_grid_preserves_overview_context_full_row_at_1100(cx: &mut TestAppContext) {
    // Existing Overview context keeps its full content extent when chart rows gain track counts.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.prepare_overview_grid_context(1100.0);

    let bounds = harness.driver.overview_grid_context_bounds();

    expect_overview_grid_context_span(&bounds);
    harness.teardown();
}

#[gpui::test]
fn apps_grid_reflows_the_same_named_selection_across_both_breakpoints(cx: &mut TestAppContext) {
    // Resize one installed selection through every approved media band and back to wide.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.prepare_apps_grid(1100.0);
    let retained = harness.driver.apps_grid_reading();
    expect_initial_apps_grid_selection(&retained);
    expect_apps_grid(&harness.driver.apps_grid_bounds(), 3);

    harness.driver.resize_apps_grid(1099.0);
    expect_apps_grid(&harness.driver.apps_grid_bounds(), 2);
    expect_retained_apps_grid_selection(&harness.driver.apps_grid_reading(), &retained);
    harness.driver.resize_apps_grid(640.0);
    expect_apps_grid(&harness.driver.apps_grid_bounds(), 2);
    expect_retained_apps_grid_selection(&harness.driver.apps_grid_reading(), &retained);
    harness.driver.resize_apps_grid(639.0);
    expect_apps_grid(&harness.driver.apps_grid_bounds(), 1);
    expect_retained_apps_grid_selection(&harness.driver.apps_grid_reading(), &retained);
    harness.driver.resize_apps_grid(1100.0);

    expect_apps_grid(&harness.driver.apps_grid_bounds(), 3);
    expect_retained_apps_grid_selection(&harness.driver.apps_grid_reading(), &retained);
    harness.teardown();
}

#[gpui::test]
fn apps_grid_empty_third_track_owns_no_history_hitbox(cx: &mut TestAppContext) {
    // Move from a named history into the unoccupied third column of its final row.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.prepare_apps_grid(1100.0);
    let bounds = harness.driver.apps_grid_bounds();
    let empty = gpui::point(bounds.named[2].center().x, bounds.named[7].center().y);
    let retained = harness.driver.apps_grid_reading();
    harness.driver.hover_named_process_history();
    expect_historical_apps_grid_reading(&harness.driver.apps_grid_reading(), &retained);

    harness.driver.move_apps_grid_pointer(empty);

    expect_current_apps_grid_reading(&harness.driver.apps_grid_reading(), &retained);
    expect_empty_apps_grid_track(&bounds, empty);
    assert!(
        !harness
            .driver
            .text_is_visible("chart-tooltip:live-process-group-cpu")
    );
    harness.teardown();
}

#[gpui::test]
fn apps_grid_exact_hover_retains_context_and_restores_readings_on_exit(cx: &mut TestAppContext) {
    // Keep the same group context while actual plot input loads and leaves exact details.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.prepare_apps_grid(1100.0);
    let before = harness.driver.apps_grid_bounds();
    let retained = harness.driver.apps_grid_reading();

    harness.driver.hover_named_process_history();

    expect_historical_apps_grid_reading(&harness.driver.apps_grid_reading(), &retained);
    assert!(
        harness
            .driver
            .text_is_visible("chart-reading:live-process-group-cpu:120.0 %")
    );
    assert!(
        harness
            .driver
            .text_is_visible("chart-reading:live-app-cpu:13.0 %")
    );
    harness.driver.deliver_historical_details();
    expect_historical_apps_grid_reading(&harness.driver.apps_grid_reading(), &retained);
    assert!(
        harness
            .driver
            .text_is_visible("Process group history · Accepted worker · last 15 minutes")
    );
    assert!(harness.driver.text_is_visible("Historical worker"));
    expect_apps_grid_anchor(&before, &harness.driver.apps_grid_bounds());
    harness.driver.leave_apps_grid_history();
    expect_current_apps_grid_reading(&harness.driver.apps_grid_reading(), &retained);
    assert!(
        harness
            .driver
            .text_is_visible("chart-reading:live-process-group-cpu:240.0 %")
    );
    assert!(
        harness
            .driver
            .text_is_visible("chart-reading:live-app-cpu:87.0 %")
    );
    expect_apps_grid_anchor(&before, &harness.driver.apps_grid_bounds());
    harness.teardown();
}

fn expect_apps_grid(bounds: &AppsGridBounds, columns: usize) {
    expect_apps_grid_tracks(&bounds.named, columns);
    expect_apps_grid_tracks(&bounds.app, columns.min(2));
    assert!(bounds.executable.unwrap().bottom() < bounds.named[0].top());
    assert!(
        bounds
            .named
            .iter()
            .all(|card| card.bottom() < bounds.totals.top())
    );
    assert!(bounds.totals.bottom() < bounds.app[0].top());
    assert!(
        bounds
            .app
            .iter()
            .all(|card| card.bottom() < bounds.instances.top())
    );
}

fn expect_apps_grid_tracks(cards: &[Bounds<Pixels>], columns: usize) {
    assert!((1..=3).contains(&columns));
    let width = cards[0].size.width;
    for (index, card) in cards.iter().enumerate() {
        expect_apps_grid_coordinate(card.size.width, width);
        let column = u8::try_from(index % columns).unwrap();
        let left = cards[0].left() + (width + px(8.0)) * f32::from(column);
        expect_apps_grid_coordinate(card.left(), left);
        let first = index / columns * columns;
        expect_apps_grid_coordinate(card.top(), cards[first].top());
        if index >= columns {
            let previous = &cards[first - columns..first];
            let bottom = previous.iter().map(Bounds::bottom).max().unwrap();
            expect_apps_grid_coordinate(card.top() - bottom, px(8.0));
        }
    }
}

fn expect_apps_grid_coordinate(actual: Pixels, expected: Pixels) {
    assert!(
        (actual - expected).as_f32().abs() <= 1.0,
        "{actual:?} != {expected:?}"
    );
}

fn expect_overview_grid_context_span(bounds: &OverviewGridBounds) {
    let width = bounds.primary_last.right() - bounds.primary_first.left();
    let card_width = (width - px(8.0)) / 2.0;
    expect_apps_grid_coordinate(bounds.pressure.size.width, card_width);
    expect_apps_grid_coordinate(bounds.temperature.size.width, card_width);
    expect_apps_grid_coordinate(bounds.pressure.left(), bounds.primary_first.left());
    expect_apps_grid_coordinate(bounds.temperature.right(), bounds.primary_last.right());
    expect_apps_grid_coordinate(bounds.temperature.left() - bounds.pressure.right(), px(8.0));
}

fn expect_empty_apps_grid_track(bounds: &AppsGridBounds, pointer: Point<Pixels>) {
    assert!(bounds.named.iter().all(|card| !card.contains(&pointer)));
    assert!(bounds.app.iter().all(|card| !card.contains(&pointer)));
    assert!(pointer.x > bounds.named[7].right());
}

fn expect_historical_apps_grid_reading(reading: &AppsGridReading, retained: &AppsGridReading) {
    assert_eq!(reading.accepted_publication, Some(2));
    assert_eq!(reading.reading_second, Some(999));
    assert_eq!(reading.reading_publication, Some(1));
    assert_eq!(
        reading.preview_source.as_deref(),
        Some("live-process-group-cpu")
    );
    assert_eq!(reading.preview_load_history, Some(false));
    expect_apps_grid_context_owner(reading, retained);
}

fn expect_current_apps_grid_reading(reading: &AppsGridReading, retained: &AppsGridReading) {
    assert_eq!(reading.accepted_publication, Some(2));
    assert_eq!(reading.reading_second, Some(1000));
    assert_eq!(reading.reading_publication, Some(2));
    assert!(reading.preview_source.is_none());
    expect_apps_grid_context_owner(reading, retained);
}

fn expect_initial_apps_grid_selection(reading: &AppsGridReading) {
    assert_eq!(reading.accepted_publication, Some(2));
    assert_eq!(reading.accepted_identity, reading.reading_identity);
    assert_eq!(
        reading.app,
        Some(AppKey {
            kind: "bundle".into(),
            bytes_hex: "41".into()
        })
    );
    assert_eq!(reading.executable.as_deref(), Some("hex:61"));
    assert!(reading.chart_readings[0].ends_with("240.0 %"));
    assert!(reading.chart_readings[1].ends_with("2.0 GiB"));
    assert!(reading.chart_readings[2].ends_with("87.0 %"));
    assert!(reading.chart_readings[3].ends_with("7.0 GiB"));
}

fn expect_retained_apps_grid_selection(reading: &AppsGridReading, retained: &AppsGridReading) {
    assert_eq!(reading.accepted_identity, retained.accepted_identity);
    assert_eq!(reading.reading_identity, retained.reading_identity);
    assert_eq!(reading.app, retained.app);
    assert_eq!(reading.executable, retained.executable);
    expect_apps_grid_context_owner(reading, retained);
    assert_eq!(reading.chart_readings, retained.chart_readings);
}

fn expect_apps_grid_context_owner(reading: &AppsGridReading, retained: &AppsGridReading) {
    let context = reading
        .context
        .as_ref()
        .expect("Effective reading must retain process history");
    let original = retained
        .context
        .as_ref()
        .expect("Initial reading must contain process history");
    assert!(
        std::sync::Arc::ptr_eq(context, original),
        "Effective process history owner changed"
    );
}

fn expect_apps_grid_anchor(before: &AppsGridBounds, after: &AppsGridBounds) {
    assert_eq!(before.named, after.named);
    assert_eq!(before.app, after.app);
}

impl AppHoverDriver {
    fn prepare_overview_grid_context(&mut self, width: f32) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.request.route = "overview".into();
            dashboard.accept(AppEpoch::Accepted.snapshot(dashboard.generation));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.resize_apps_grid(width);
    }

    fn overview_grid_context_bounds(&mut self) -> std::rc::Rc<OverviewGridBounds> {
        std::rc::Rc::new(OverviewGridBounds {
            primary_first: self.context.debug_bounds("chart-card:live-cpu").unwrap(),
            primary_last: self.context.debug_bounds("chart-card:live-memory").unwrap(),
            pressure: self
                .context
                .debug_bounds("chart-card:live-memory-pressure")
                .unwrap(),
            temperature: self
                .context
                .debug_bounds("chart-card:live-temperature")
                .unwrap(),
        })
    }

    fn prepare_apps_grid(&mut self, width: f32) {
        self.resize_apps_grid(1536.0);
        self.process_history_control("executable-disk:hex:61:Unavailable *");
        self.deliver_named_process_history();
        self.resize_apps_grid(width);
    }

    fn resize_apps_grid(&mut self, width: f32) {
        self.context
            .simulate_resize(gpui::size(px(width), px(8000.0)));
        self.context.run_until_parked();
    }

    fn apps_grid_bounds(&mut self) -> Box<AppsGridBounds> {
        Box::new(AppsGridBounds {
            named: [
                "chart-card:live-process-group-cpu",
                "chart-card:live-process-group-resident",
                "chart-card:live-process-group-footprint",
                "chart-card:live-process-group-power",
                "chart-card:live-process-group-disk",
                "chart-card:live-process-group-wakeups",
                "chart-card:live-process-group-pageins",
                "chart-card:live-process-group-threads",
            ]
            .map(|id| self.context.debug_bounds(id).unwrap()),
            app: [
                "chart-card:live-app-cpu",
                "chart-card:live-app-resident",
                "chart-card:live-app-footprint",
                "chart-card:live-app-power",
                "chart-card:live-app-disk",
            ]
            .map(|id| self.context.debug_bounds(id).unwrap()),
            executable: self
                .context
                .debug_bounds("executable-disk:hex:61:Unavailable *"),
            totals: self
                .context
                .debug_bounds("Selected App totals · last 15 minutes")
                .unwrap(),
            instances: self
                .context
                .debug_bounds("Process instances · selected app")
                .unwrap(),
        })
    }

    fn apps_grid_reading(&self) -> std::rc::Rc<AppsGridReading> {
        self.view.read_with(&self.context, |dashboard, _| {
            let accepted = dashboard.snapshot.as_ref().unwrap();
            let reading = dashboard.reading_snapshot().unwrap();
            std::rc::Rc::new(AppsGridReading {
                accepted_publication: accepted.selection["selected"]["publication"].as_u64(),
                accepted_identity: accepted.selection["selected"].clone(),
                reading_second: reading.selection["second"].as_u64(),
                reading_publication: reading.selection["selected"]["publication"].as_u64(),
                reading_identity: reading.selection["selected"].clone(),
                app: dashboard.request.app.clone(),
                executable: dashboard.request.executable.clone(),
                preview_source: dashboard
                    .preview
                    .as_ref()
                    .map(|preview| preview.source.to_string()),
                preview_load_history: dashboard
                    .preview
                    .as_ref()
                    .map(|preview| preview.request.load_process_history),
                context: reading.process_history.as_ref().map(std::sync::Arc::clone),
                chart_readings: [
                    "live-process-group-cpu",
                    "live-process-group-resident",
                    "live-app-cpu",
                    "live-app-resident",
                ]
                .map(|id| {
                    let panel = dashboard
                        .panels
                        .iter()
                        .take(16)
                        .find(|panel| panel.id == id)
                        .unwrap();
                    dashboard.chart_accessibility_reading(panel).1
                }),
            })
        })
    }

    fn move_apps_grid_pointer(&mut self, position: Point<Pixels>) {
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
        self.context.run_until_parked();
    }

    fn leave_apps_grid_history(&mut self) {
        let bounds = self
            .context
            .debug_bounds("live-process-group-cpu-plot")
            .unwrap();
        self.move_apps_grid_pointer(gpui::point(px(20.0), bounds.center().y));
    }
}
