use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::{Value, json};

use crate::live_dashboard::LiveDashboard;
use crate::live_source::tests::rankings::app_parts::kept_window_parts;
use crate::live_source::{AppKey, Snapshot, Update, Worker};

#[path = "live_app_disk_input_tests.rs"]
mod disk;
#[path = "live_process_history_input_tests.rs"]
mod process_history;

#[gpui::test]
fn hovering_an_app_replaces_current_details_with_the_exact_historical_page(
    cx: &mut TestAppContext,
) {
    // Hover the real chart and deliver exact historical details while reading the composed frame.
    let mut harness = AppHoverHarness::setup(cx);
    assert!(harness.driver.text_is_visible("87%"));
    assert!(harness.driver.text_is_visible("Accepted worker"));
    assert!(
        harness
            .driver
            .text_is_visible("summary-card:Process power:9 W")
    );

    harness.driver.hover_previous_second();

    assert!(harness.driver.text_is_visible("13%"));
    assert!(!harness.driver.text_is_visible("Accepted worker"));
    assert!(
        !harness
            .driver
            .text_is_visible("summary-card:Process power:9 W")
    );
    harness.driver.deliver_historical_details();

    assert!(harness.driver.text_is_visible("13%"));
    assert!(harness.driver.text_is_visible("Historical worker"));
    assert!(harness.driver.text_is_visible("120.0%"));
    assert!(harness.driver.text_is_visible("0.12 GiB"));
    assert!(
        harness
            .driver
            .text_is_visible("summary-card:Process power:4 W")
    );
    assert!(!harness.driver.text_is_visible("Accepted worker"));
    assert!(!harness.driver.text_is_visible("Accepted helper"));
    harness.teardown();
}

#[gpui::test]
fn hovering_with_the_app_picker_open_reads_a_second_whose_app_part_is_kept(
    cx: &mut TestAppContext,
) {
    // Goal: the open App picker lists the hovered second's Apps, which only a read gives, even
    // where the page keeps the selected App's part of that second.
    // Method: with the picker open, select a sealed recording's App whose window parts the page
    // keeps, then hover the earlier second and tick the hover.
    let mut harness = AppHoverHarness::setup(cx);
    harness
        .driver
        .keep_app_parts("App00600", kept_window_parts("App00600"));

    harness.driver.hover_previous_second();
    harness.driver.tick_hover();

    assert_eq!(harness.driver.hover_reads(), 1);
    harness.teardown();
}

#[gpui::test]
fn sub_watt_app_power_reads_in_milliwatts(cx: &mut TestAppContext) {
    // Goal: the card keeps the prototype's milliwatt precision instead of rounding to 0.1 W.
    // Method: accept an App drawing 0.142 W and read its Process power card.
    let mut harness = AppHoverHarness::setup(cx);

    harness.driver.seed_app_power(0.142);

    assert!(
        harness
            .driver
            .text_is_visible("summary-card:Process power:142 mW")
    );
    harness.teardown();
}

#[gpui::test]
fn the_selected_app_reads_in_six_summary_cards(cx: &mut TestAppContext) {
    // Goal: Apps opens on the prototype's six readings of the selected App.
    // Method: accept the fixture App and find each card's label and reading in the frame.
    let mut harness = AppHoverHarness::setup(cx);
    for card in [
        "summary-card:CPU:87%",
        "summary-card:Resident memory:7 GiB",
        "summary-card:Network:Unavailable",
        "summary-card:Disk I/O:Unavailable *",
        "summary-card:GPU time:25%",
        "summary-card:Process power:9 W",
    ] {
        assert!(harness.driver.text_is_visible(card), "{card}");
    }
    harness.teardown();
}

#[gpui::test]
fn the_selected_app_totals_read_as_the_prototypes_app_named_charts(cx: &mut TestAppContext) {
    // Goal: the totals chart the selected App's CPU, resident memory, disk, GPU time and power
    // under its name, one line each, and say why the prototype's network chart is missing.
    // Method: accept the fixture App and read the installed totals panels and the note.
    let mut harness = AppHoverHarness::setup(cx);

    harness
        .driver
        .view
        .read_with(&harness.driver.context, |dashboard, _| {
            let totals: Vec<_> = dashboard
                .panels
                .iter()
                .filter(|panel| panel.id.starts_with("live-app-"))
                .collect();
            let titles: Vec<&str> = totals.iter().map(|panel| panel.title.as_str()).collect();
            assert_eq!(
                titles,
                [
                    "CPU · Example App",
                    "Resident memory · Example App",
                    "Network · Example App",
                    "Disk I/O · Example App",
                    "GPU time · Example App",
                    "Process power · Example App",
                ]
            );
            for panel in totals {
                assert_eq!(panel.lines.len(), 1, "{}", panel.id);
                assert_eq!(panel.lines[0].name, "Example App", "{}", panel.id);
            }
        });
    assert!(harness.driver.text_is_visible("chart-card:live-app-gpu"));
    harness.teardown();
}

#[gpui::test]
fn the_app_picker_counts_the_process_groups_at_the_selected_second(cx: &mut TestAppContext) {
    // Goal: the App picker reads like the prototype's, with the groups it shows and their time.
    // Method: accept the fixture App, whose three processes form two groups, and find the line.
    let mut harness = AppHoverHarness::setup(cx);
    let context: &'static str = format!(
        "app-context:2 process groups at {}",
        crate::live_presentation::clock(1000, true)
    )
    .leak();
    assert!(
        harness.driver.context.debug_bounds(context).is_some(),
        "{context}"
    );
    harness.teardown();
}

#[gpui::test]
fn the_process_group_table_reads_in_the_prototypes_columns(cx: &mut TestAppContext) {
    // Goal: the table inside the App has the prototype's title, note and columns, and each
    // reading sits under its column.
    // Method: accept the fixture App, find the title, note and headings, and align Disk I/O.
    let mut harness = AppHoverHarness::setup(cx);
    for text in [
        "Inside Example App",
        "Grouped by process name, not PID",
        "app-column:executables:Process group",
        "app-column:executables:Network",
        "app-column:executables:GPU time",
        "app-column:executables:Wakeups/s",
        "app-column:executables:Open files",
        "app-column:executables:Sockets",
        "About process grouping and snapshot coverage",
    ] {
        assert!(harness.driver.text_is_visible(text), "{text}");
    }
    let heading = harness
        .driver
        .context
        .debug_bounds("app-column:executables:Disk I/O")
        .unwrap();
    let reading = harness
        .driver
        .context
        .debug_bounds("executable-disk:hex:61:— *")
        .unwrap();
    assert_eq!(reading.left(), heading.left());
    harness.teardown();
}

#[gpui::test]
fn historical_app_details_keep_the_graph_under_a_stationary_pointer(cx: &mut TestAppContext) {
    // Vary preceding process/group counts and compare rendered chart bounds without another move.
    let mut harness = AppHoverHarness::setup(cx);
    let accepted = harness.driver.chart_bounds();

    harness.driver.hover_previous_second();
    let pending = harness.driver.chart_bounds();
    assert_eq!(
        pending, accepted,
        "Pending details must retain the chart's position"
    );

    harness.driver.deliver_historical_details();
    let ready = harness.driver.chart_bounds();

    assert_eq!(
        ready, accepted,
        "Historical row counts must retain the chart's position"
    );
    assert!(harness.driver.text_is_visible("13%"));
    harness.teardown();
}

#[gpui::test]
fn growing_historical_app_details_keep_the_graph_under_a_stationary_pointer(
    cx: &mut TestAppContext,
) {
    // Grow the preceding groups and processes while keeping the actual plot stationary.
    let mut harness = AppHoverHarness::setup_history(cx, AppEpoch::LargerHistorical);
    let accepted = harness.driver.chart_bounds();

    harness.driver.hover_previous_second();
    assert_eq!(harness.driver.chart_bounds(), accepted);
    harness.driver.deliver_historical_details();

    assert_eq!(harness.driver.chart_bounds(), accepted);
    assert!(harness.driver.text_is_visible("13%"));
    assert!(
        harness
            .driver
            .text_is_visible("summary-card:Process power:4 W")
    );
    assert!(!harness.driver.text_is_visible("Accepted worker"));
    harness.teardown();
}

#[gpui::test]
fn scrolled_app_details_keep_the_graph_under_a_stationary_pointer(cx: &mut TestAppContext) {
    // Exercise the same replacement after a real wheel scroll in a normal-height window.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.scroll_to_history();
    let accepted = harness.driver.chart_bounds();

    harness.driver.hover_previous_second();
    assert_eq!(harness.driver.chart_bounds(), accepted);
    harness.driver.deliver_historical_details();

    assert_eq!(harness.driver.chart_bounds(), accepted);
    assert!(harness.driver.text_is_visible("13%"));
    harness.teardown();
}

#[gpui::test]
fn a_wheel_before_the_next_frame_scrolls_from_where_a_hover_left_the_graph(
    cx: &mut TestAppContext,
) {
    // Goal: the wheel moves the page by its own distance even when a hover's end moved the page's
    // rows since the window last drew, so the graph never jumps as the pointer crosses it.
    // Method: end a hover and turn the wheel in one turn, before the window draws, then compare
    // the graph's position with where the hover held it.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.scroll_to_history();
    harness.driver.hover_previous_second();
    harness.driver.deliver_historical_details();
    let hovered = harness.driver.chart_bounds();

    harness
        .driver
        .leave_and_wheel_before_drawing(gpui::px(24.0));

    assert_eq!(
        harness.driver.chart_bounds().top(),
        hovered.top() - gpui::px(24.0)
    );
    harness.teardown();
}

struct AppHoverHarness {
    driver: AppHoverDriver,
}

macro_rules! vertical_table_wheel_case {
    ($name:ident, $row:pat, $selector:literal) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Keep horizontal columns fixed while real vertical input scrolls the outer view.
            let mut harness = AppHoverHarness::setup(cx);

            harness
                .driver
                .vertical_wheel_keeps_table_columns(|row| matches!(row, $row), $selector);

            harness.teardown();
        }
    };
}

vertical_table_wheel_case!(
    vertical_wheel_over_app_picker_keeps_columns_and_scrolls_outer_view,
    crate::live_dashboard::Row::App(0),
    "app-disk:bundle:41:Unavailable *"
);
vertical_table_wheel_case!(
    vertical_wheel_over_executable_keeps_columns_and_scrolls_outer_view,
    crate::live_dashboard::Row::Executable(0),
    "executable-disk:hex:61:— *"
);

#[gpui::test]
fn leaving_app_history_restores_the_real_end_of_the_recording_view(cx: &mut TestAppContext) {
    // A completed preview must remove its reserved tail rather than leave a blank scroll viewport.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.deliver_historical_details();

    harness.driver.leave_and_scroll_to_end();

    assert!(harness.driver.text_is_visible("87%"));
    assert!(harness.driver.last_chart_is_visible());
    harness.teardown();
}

#[gpui::test]
fn a_page_of_apps_while_a_second_is_held_keeps_the_charts_and_reads_the_page_up_to_the_hold(
    cx: &mut TestAppContext,
) {
    // Goal: the next page of the Apps table while a second is held keeps the charts drawn at the
    // click until the reply, and asks the reader for the page whose context ends where the
    // charts were drawn, which carries the App and the table, rather than emptying the page.
    // Method: the Apps page over two seconds, a click on the earlier second of the App's CPU
    // chart, the reader's answer to the read of that second with a second page of Apps, the
    // pointer off the chart, Next Apps, then whether the CPU chart is drawn, the second the page
    // shows, and the second, reading, App and page of Apps the reader was asked for.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.hold_previous_second();
    harness.driver.answer_held_read_with_more_apps();
    harness.driver.leave_history();

    harness.driver.next_apps();

    assert!(harness.driver.cpu_chart_is_drawn());
    assert_eq!(harness.driver.shown_reading_second(), Some(999));
    let asked = harness.driver.reader_asked().unwrap();
    assert_eq!(asked.second, Some(1000 - viewer::CONTEXT_AFTER_SECONDS));
    assert_eq!(asked.identity, None);
    assert_eq!(asked.app, Some(open_app()));
    assert_eq!(asked.apps_offset, 512);
    harness.teardown();
}

impl AppHoverHarness {
    fn setup(cx: &mut TestAppContext) -> Self {
        Self::setup_history(cx, AppEpoch::Historical)
    }

    fn setup_history(cx: &mut TestAppContext, historical: AppEpoch) -> Self {
        let root =
            std::env::temp_dir().join(format!("flightdeck-app-hover-input-{}", std::process::id()));
        let worker = Worker::start(root).unwrap();
        let window = cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |_, cx| {
                crate::native_workflows::create_dashboard(worker, "apps".into(), None, cx)
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let context = VisualTestContext::from_window(window.into(), cx);
        let mut harness = Self {
            driver: AppHoverDriver {
                view,
                context,
                historical,
            },
        };
        harness.driver.seed_accepted_app();
        harness
    }

    fn teardown(self) {
        drop(self);
    }
}

impl Drop for AppHoverHarness {
    fn drop(&mut self) {
        self.driver.context.update(|window, cx| {
            self.driver
                .view
                .update(cx, |dashboard, _| dashboard.worker.stop());
            window.remove_window();
        });
        self.driver.context.run_until_parked();
    }
}

struct AppHoverDriver {
    view: Entity<LiveDashboard>,
    context: VisualTestContext,
    historical: AppEpoch,
}

impl AppHoverDriver {
    fn vertical_wheel_keeps_table_columns(
        &mut self,
        target: fn(&crate::live_dashboard::Row) -> bool,
        selector: &'static str,
    ) {
        self.seed_scrollable_process_page();
        self.context
            .simulate_resize(gpui::size(gpui::px(640.0), gpui::px(900.0)));
        self.context.run_until_parked();
        let index = self.view.update(&mut self.context, |dashboard, cx| {
            let index = dashboard.rows.iter().position(target).unwrap();
            dashboard.list.scroll_to(gpui::ListOffset {
                item_ix: index.saturating_sub(2),
                offset_in_item: gpui::px(0.0),
            });
            cx.notify();
            index
        });
        self.context.run_until_parked();
        let before = self.context.debug_bounds(selector).unwrap();
        let row = self.view.read_with(&self.context, |dashboard, _| {
            dashboard.list.bounds_for_item(index).unwrap()
        });
        let offset = self.view.read_with(&self.context, |dashboard, _| {
            dashboard.list.scroll_px_offset_for_scrollbar()
        });
        let viewport = self.view.read_with(&self.context, |dashboard, _| {
            dashboard.list.viewport_bounds()
        });
        assert!(viewport.contains(&row.center()));
        self.context.simulate_event(gpui::ScrollWheelEvent {
            position: row.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-48.0))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        self.context.run_until_parked();
        let after = self.context.debug_bounds(selector).unwrap();
        let moved = self.view.read_with(&self.context, |dashboard, _| {
            dashboard.list.bounds_for_item(index).unwrap()
        });
        let scrolled = self.view.read_with(&self.context, |dashboard, _| {
            dashboard.list.scroll_px_offset_for_scrollbar()
        });
        eprintln!(
            "QA_VERTICAL_WHEEL selector={selector} viewport={viewport:?} \
             row_before={row:?} row_after={moved:?} column_before={before:?} \
             column_after={after:?} outer_before={offset:?} outer_after={scrolled:?}"
        );
        assert_eq!(
            after.left(),
            before.left(),
            "Vertical wheel shifted {selector}"
        );
        assert!(
            moved.top() < row.top(),
            "Vertical wheel did not scroll the outer view"
        );
    }

    fn seed_app_power(&mut self, watts: f64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = AppEpoch::Accepted.snapshot(dashboard.generation);
            let detail = snapshot.detail.as_mut().unwrap().legacy_mut().unwrap();
            detail["app"]["energy_watts"] = json!(watts);
            snapshot.apps.as_mut().unwrap().legacy_mut().unwrap()["apps"][0] =
                detail["app"].clone();
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn seed_scrollable_process_page(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = AppEpoch::Accepted.snapshot(dashboard.generation);
            let detail = snapshot.detail.as_mut().unwrap().legacy_mut().unwrap();
            for pid in 100..150 {
                detail["processes"].as_array_mut().unwrap().push(process(
                    "Accepted worker",
                    "61",
                    pid,
                ));
            }
            detail["app"]["members"] = json!(53);
            detail["app"]["live_members"] = json!(53);
            detail["page"]["total"] = json!(53);
            snapshot.apps.as_mut().unwrap().legacy_mut().unwrap()["apps"][0] =
                detail["app"].clone();
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn leave_history(&mut self) {
        let bounds = self.chart_bounds();
        self.context.simulate_mouse_move(
            gpui::point(gpui::px(20.0), bounds.center().y),
            None,
            Modifiers::none(),
        );
    }

    fn seed_accepted_app(&mut self) {
        let historical = self.historical;
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.request.second = Some(1000);
            dashboard.request.identity = Some(identity(2));
            dashboard.request.app = Some(open_app());
            dashboard.picker = Some(crate::live_dashboard::Picker::App);
            let mut snapshot = AppEpoch::Accepted.snapshot(dashboard.generation);
            snapshot.history.as_mut().unwrap().legacy_mut().unwrap()["points"][0] =
                historical.history_point();
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2400.0)));
        self.context.run_until_parked();
    }

    fn hover_previous_second(&mut self) {
        let position = self.previous_second_position();
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
    }

    /// Clicks the second before the newest on the App's CPU chart, as a person holds a second.
    fn hold_previous_second(&mut self) {
        self.hover_previous_second();
        let position = self.previous_second_position();
        self.context.simulate_click(position, Modifiers::none());
        self.context.run_until_parked();
    }

    /// Where the second before the newest lies on the App's CPU chart.
    fn previous_second_position(&mut self) -> gpui::Point<gpui::Pixels> {
        let plot = crate::charts::plot_bounds(self.chart_bounds());

        gpui::point(
            plot.left() + plot.size.width * (898.5 / 900.0),
            plot.center().y,
        )
    }

    /// Hands the page the reader's answer to its read of the held second, whose Apps table has a
    /// next page of Apps.
    fn answer_held_read_with_more_apps(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let generation = dashboard.held_read_generation().unwrap();
            let mut snapshot = AppEpoch::Historical.snapshot(generation);
            let apps = snapshot.apps.as_mut().unwrap().legacy_mut().unwrap();
            apps["page"]["total"] = json!(513);
            apps["page"]["next_offset"] = json!(512);
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Clicks the control that shows the next page of the Apps table.
    fn next_apps(&mut self) {
        let bounds = self.context.debug_bounds("Next Apps").unwrap();
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }

    /// Whether the App's CPU chart is drawn.
    fn cpu_chart_is_drawn(&mut self) -> bool {
        self.context.debug_bounds("live-app-cpu-plot").is_some()
    }

    /// The second the page's readings show.
    fn shown_reading_second(&self) -> Option<u64> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.reading_target().map(|target| target.second)
        })
    }

    /// The request of the window's last command to the reader.
    fn reader_asked(&self) -> Option<crate::live_source::Request> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.worker.sent_request()
        })
    }

    fn keep_app_parts(&mut self, app: &str, parts: Arc<crate::live_source::PartsWindow>) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.request.app = Some(AppKey {
                kind: "executable".into(),
                bytes_hex: crate::live_source::app_key_bytes_hex(app.as_bytes()),
            });
            dashboard.snapshot.as_mut().unwrap().app_parts = Some(parts);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn tick_hover(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
        });
        self.context.run_until_parked();
    }

    fn hover_reads(&self) -> u64 {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.preview_submissions)
    }

    fn deliver_historical_details(&mut self) {
        let historical = self.historical;
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
            let generation = dashboard.preview.as_ref().unwrap().generation.unwrap();
            assert!(dashboard.preview_receive(Update::Ready(historical.snapshot(generation))));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn scroll_to_history(&mut self) {
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(900.0)));
        self.context.run_until_parked();
        self.context.simulate_event(gpui::ScrollWheelEvent {
            position: gpui::point(gpui::px(185.0), gpui::px(700.0)),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-700.0))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        self.context.run_until_parked();
        let plot = crate::charts::plot_bounds(self.chart_bounds());
        assert!(plot.top() >= gpui::px(0.0));
        assert!(plot.bottom() <= gpui::px(900.0));
    }

    /// Moves the pointer off the graph's plot onto its card and turns the wheel there by
    /// `distance` in the same turn, so the window draws neither before the wheel.
    fn leave_and_wheel_before_drawing(&mut self, distance: gpui::Pixels) {
        let card = self.chart_bounds();
        let position = gpui::point(card.left() + gpui::px(2.0), card.center().y);
        self.context.update(|window, cx| {
            let moved = gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers: Modifiers::none(),
            });
            window.dispatch_event(moved, cx);
            let wheel = gpui::PlatformInput::ScrollWheel(gpui::ScrollWheelEvent {
                position,
                delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), -distance)),
                modifiers: Modifiers::none(),
                touch_phase: gpui::TouchPhase::Moved,
            });
            window.dispatch_event(wheel, cx);
        });
        self.context.run_until_parked();
    }

    fn leave_and_scroll_to_end(&mut self) {
        self.context.simulate_mouse_move(
            gpui::point(gpui::px(20.0), gpui::px(1000.0)),
            None,
            Modifiers::none(),
        );
        self.context.simulate_event(gpui::ScrollWheelEvent {
            position: gpui::point(gpui::px(185.0), gpui::px(1000.0)),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-10000.0))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        self.context.run_until_parked();
    }

    fn text_is_visible(&mut self, text: &'static str) -> bool {
        self.context.debug_bounds(text).is_some()
    }

    fn last_chart_is_visible(&mut self) -> bool {
        self.context
            .debug_bounds("chart-card:live-app-power")
            .is_some()
    }

    fn chart_bounds(&mut self) -> gpui::Bounds<gpui::Pixels> {
        self.context.debug_bounds("live-app-cpu-plot").unwrap()
    }
}

#[derive(Clone, Copy)]
enum AppEpoch {
    Accepted,
    Historical,
    LargerHistorical,
}

impl AppEpoch {
    fn snapshot(self, generation: u64) -> Box<Snapshot> {
        let (second, publication) = match self {
            Self::Accepted => (1000, 2),
            Self::Historical | Self::LargerHistorical => (999, 1),
        };
        let selected = identity(publication);
        let app = self.app();
        let points = json!([
            Self::Historical.history_point(),
            Self::Accepted.history_point()
        ]);
        Box::new(Snapshot {
            generation,
            catalog: json!({"path":"/fixture/app-hover","first_second":999,"last_second":1000}),
            selection: crate::live_source::Presentation::Legacy(
                json!({"second":second,"context_last_second":second,"selected":selected,
                "summary":null,"trends":[system_point(999,1),system_point(1000,2)]}),
            ),
            pages: BTreeMap::new(),
            apps: Some(crate::live_source::Presentation::Legacy(
                json!({"selected":selected,"apps":[app],"ungrouped_members":0,
                "page":{"offset":0,"total":1,"next_offset":null}}),
            )),
            detail: Some(crate::live_source::Presentation::Legacy(
                json!({"selected":selected,"app":app,"elapsed_ns":"1000000000",
                "processes":self.processes(),
                "page":{"offset":0,"total":app["members"],"next_offset":null}}),
            )),
            history: Some(crate::live_source::Presentation::Legacy(
                json!({"selected":wire_observation(second,publication),
                "context_seconds":900,"points":points}),
            )),
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

    fn app(self) -> Value {
        let (members, cpu, memory, energy) = match self {
            Self::Accepted => (3, 87, "7516192768", 9),
            Self::Historical => (2, 13, "3221225472", 4),
            Self::LargerHistorical => (5, 13, "3221225472", 4),
        };
        json!({"display_name":"Example App","key":{"kind":"bundle","bytes_hex":"41",
            "display":"Example App"},"members":members,"live_members":members,
            "membership_complete":true,"cpu_percent_one_core":cpu,
            "energy_watts":energy,"energy_joules":energy,
            "cpu_ns":measure("130000000"),"energy_nj":measure("4000000000"),
            "gpu_ns":measure("250000000"),"gpu_percent":25.0,
            "resident_bytes":measure(memory),"footprint_bytes":measure(memory),
            "disk_read_bytes":{"known":"0","measured_members":0,
                "unavailable_members":members,"overflowed":false},
            "disk_written_bytes":{"known":"0","measured_members":0,
                "unavailable_members":members,"overflowed":false},
            "disk_io_bytes":{"known":"0","measured_members":0,
                "unavailable_members":members,"overflowed":false},
            "disk_read_bps":null,"disk_written_bps":null,"disk_io_bps":null})
    }

    fn history_point(self) -> Value {
        let (second, publication) = match self {
            Self::Accepted => (1000, 2),
            Self::Historical | Self::LargerHistorical => (999, 1),
        };
        let mut point = self.app();
        point["observation"] = wire_observation(second, publication);
        point["segment"] = json!(0);
        point
    }

    fn processes(self) -> Value {
        match self {
            Self::Accepted => json!([
                process("Accepted worker", "61", 10),
                process("Accepted worker", "61", 11),
                process("Accepted helper", "62", 12)
            ]),
            Self::Historical => json!([
                process("Historical worker", "63", 20),
                process("Historical worker", "63", 21)
            ]),
            Self::LargerHistorical => json!([
                process("Historical worker", "63", 20),
                process("Historical worker", "63", 21),
                process("Historical worker", "63", 22),
                process("Historical helper", "64", 23),
                process("Historical renderer", "65", 24)
            ]),
        }
    }
}

/// The App the page has open, the bundle whose key is the byte `A`.
fn open_app() -> AppKey {
    AppKey {
        kind: "bundle".into(),
        bytes_hex: "41".into(),
    }
}

fn identity(publication: u64) -> Value {
    json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004","publication":publication})
}

fn wire_observation(second: u64, publication: u64) -> Value {
    json!({"stamp":identity(publication),"wall_ns":second * 1_000_000_000})
}

fn system_point(second: u64, publication: u64) -> Value {
    let mut point = identity(publication);
    point["wall_ns"] = json!(second * 1_000_000_000);
    point["segment"] = json!(0);
    point
}

fn measure(known: &str) -> Value {
    json!({"known":known,"measured_members":1,"unavailable_members":0,"overflowed":false})
}

fn process(name: &str, bytes: &str, pid: u32) -> Value {
    json!({"pid":pid,"creation":{"unique_id":pid},"state":"live",
        "executable_name":{"display":name,"bytes_hex":bytes},"app_attribution":null,
        "cpu_percent_one_core":60.0,"energy_watts":2.0,"energy_joules":2.0,
        "cpu_ns":{"known":"600000000","unavailable":null},
        "energy_nj":{"known":"2000000000","unavailable":null},
        "resident_bytes":{"known":"67108864","unavailable":null},
        "footprint_bytes":{"known":"134217728","unavailable":null},
        "disk_read_bytes":{"known":null,"unavailable":"interval"},
        "disk_written_bytes":{"known":null,"unavailable":"interval"},
        "disk_io_bytes":{"known":null,"unavailable":"interval"},
        "idle_wakeups":{"known":"6","unavailable":null},
        "pageins":{"known":"4","unavailable":null},"threads":{"known":"8","unavailable":null},
        "gpu_ns":{"known":null,"unavailable":"unknown"},
        "idle_wakeups_per_second":6.0,"pageins_per_second":4.0,
        "disk_read_bps":null,"disk_written_bps":null,"disk_io_bps":null})
}
