use std::collections::BTreeMap;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::{Value, json};

use crate::live_dashboard::LiveDashboard;
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
    assert!(harness.driver.text_is_visible("87.0 %"));
    assert!(harness.driver.text_is_visible("Accepted worker"));
    assert!(
        harness
            .driver
            .text_is_visible("3 live of 3 members · membership complete · 9.0 J this interval")
    );

    harness.driver.hover_previous_second();

    assert!(harness.driver.text_is_visible("13.0 %"));
    assert!(!harness.driver.text_is_visible("Accepted worker"));
    assert!(
        !harness
            .driver
            .text_is_visible("3 live of 3 members · membership complete · 9.0 J this interval")
    );
    harness.driver.deliver_historical_details();

    assert!(harness.driver.text_is_visible("13.0 %"));
    assert!(harness.driver.text_is_visible("Historical worker"));
    assert!(harness.driver.text_is_visible("120.0 %"));
    assert!(harness.driver.text_is_visible("64.0 MiB"));
    assert!(
        harness
            .driver
            .text_is_visible("2 live of 2 members · membership complete · 4.0 J this interval")
    );
    assert!(!harness.driver.text_is_visible("Accepted worker"));
    assert!(!harness.driver.text_is_visible("Accepted helper"));
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
    assert!(harness.driver.text_is_visible("13.0 %"));
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
    assert!(harness.driver.text_is_visible("13.0 %"));
    assert!(
        harness
            .driver
            .text_is_visible("5 live of 5 members · membership complete · 4.0 J this interval")
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
    assert!(harness.driver.text_is_visible("13.0 %"));
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
    vertical_wheel_over_process_keeps_columns_and_scrolls_outer_view,
    crate::live_dashboard::Row::Process(0),
    "process-disk:10:Unavailable"
);
vertical_table_wheel_case!(
    vertical_wheel_over_heading_keeps_columns_and_scrolls_outer_view,
    crate::live_dashboard::Row::AppColumns("processes"),
    "app-column:processes:Disk I/O"
);
vertical_table_wheel_case!(
    vertical_wheel_over_app_picker_keeps_columns_and_scrolls_outer_view,
    crate::live_dashboard::Row::App(0),
    "app-disk:bundle:41:Unavailable *"
);
vertical_table_wheel_case!(
    vertical_wheel_over_executable_keeps_columns_and_scrolls_outer_view,
    crate::live_dashboard::Row::Executable(0),
    "executable-disk:hex:61:Unavailable *"
);

#[gpui::test]
fn scrolling_a_process_keeps_its_disk_column_aligned_with_the_heading(cx: &mut TestAppContext) {
    // Scroll a narrow table through real wheel input and compare the displayed column positions.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.scroll_process_horizontally();

    let heading = harness
        .driver
        .context
        .debug_bounds("app-column:processes:Disk I/O")
        .unwrap();
    let reading = harness
        .driver
        .context
        .debug_bounds("process-disk:10:Unavailable")
        .unwrap();
    assert_eq!(reading.left(), heading.left());
    assert_eq!(reading.size.width, heading.size.width);
    assert!(reading.right() <= gpui::px(640.0));
    let next = harness
        .driver
        .context
        .debug_bounds("process-disk:11:Unavailable")
        .unwrap();
    assert_eq!(next.left(), heading.left());
    harness.teardown();
}

#[gpui::test]
fn leaving_history_restores_the_expanded_process_without_moving_the_graph(cx: &mut TestAppContext) {
    // Expand a real process disclosure, then restore it across preview while scrolled mid-view.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.expand_last_process();
    harness.driver.scroll_to_history();
    let accepted = harness.driver.chart_bounds();

    harness.driver.hover_previous_second();
    assert_eq!(harness.driver.chart_bounds(), accepted);
    assert!(!harness.driver.expanded_json_is_visible());
    harness.driver.deliver_historical_details();
    assert_eq!(harness.driver.chart_bounds(), accepted);
    harness.driver.leave_history();

    assert_eq!(harness.driver.chart_bounds(), accepted);
    assert!(harness.driver.text_is_visible("87.0 %"));
    harness.driver.scroll_to_processes("expanded-json");
    assert!(harness.driver.expanded_json_is_visible());
    harness.teardown();
}

#[gpui::test]
fn leaving_app_history_restores_the_real_end_of_the_recording_view(cx: &mut TestAppContext) {
    // A completed preview must remove its reserved tail rather than leave a blank scroll viewport.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.hover_previous_second();
    harness.driver.deliver_historical_details();

    harness.driver.leave_and_scroll_to_end();

    assert!(harness.driver.text_is_visible("87.0 %"));
    assert!(harness.driver.last_process_is_visible());
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

    fn scroll_process_horizontally(&mut self) {
        self.context
            .simulate_resize(gpui::size(gpui::px(640.0), gpui::px(2400.0)));
        self.context.run_until_parked();
        self.scroll_to_processes("process-0");
        let row = self.context.debug_bounds("process-0").unwrap();
        self.context.simulate_event(gpui::ScrollWheelEvent {
            position: row.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(-1000.0), gpui::px(0.0))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        self.context.run_until_parked();
    }

    fn scroll_to_processes(&mut self, selector: &'static str) {
        let position = self.view.read_with(&self.context, |dashboard, _| {
            let viewport = dashboard.list.viewport_bounds();
            gpui::point(viewport.left() + gpui::px(8.0), viewport.center().y)
        });
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
        // Virtual rows gain their measured heights as successive real wheel events reveal them.
        for _attempt in 0..8 {
            self.context.simulate_event(gpui::ScrollWheelEvent {
                position,
                delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(-10000.0))),
                modifiers: Modifiers::none(),
                touch_phase: gpui::TouchPhase::Moved,
            });
            self.context.run_until_parked();
            if self.context.debug_bounds(selector).is_some() {
                return;
            }
        }
        panic!("Native wheel input did not reveal {selector}");
    }

    fn seed_scrollable_process_page(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = AppEpoch::Accepted.snapshot(dashboard.generation);
            let detail = snapshot.detail.as_mut().unwrap();
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
            snapshot.apps.as_mut().unwrap()["apps"][0] = detail["app"].clone();
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn expand_last_process(&mut self) {
        let row = self.context.debug_bounds("process-2").unwrap();
        self.context.simulate_click(row.center(), Modifiers::none());
        self.context.run_until_parked();
        let expanded = self.context.debug_bounds("process-2").unwrap();
        assert!(expanded.size.height > gpui::px(26.0));
        assert!(self.expanded_json_is_visible());
    }

    fn expanded_json_is_visible(&mut self) -> bool {
        self.context.debug_bounds("expanded-json").is_some()
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
            dashboard.request.app = Some(AppKey {
                kind: "bundle".into(),
                bytes_hex: "41".into(),
            });
            dashboard.app_picker_open = true;
            let mut snapshot = AppEpoch::Accepted.snapshot(dashboard.generation);
            snapshot.history.as_mut().unwrap()["points"][0] = historical.history_point();
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2400.0)));
        self.context.run_until_parked();
    }

    fn hover_previous_second(&mut self) {
        let plot = crate::charts::plot_bounds(self.chart_bounds());
        let position = gpui::point(
            plot.left() + plot.size.width * (898.5 / 900.0),
            plot.center().y,
        );
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
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

    fn last_process_is_visible(&mut self) -> bool {
        self.context.debug_bounds("process-2").is_some()
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
            selection: json!({"second":second,"selected":selected,
                "summary":null,"trends":[system_point(999,1),system_point(1000,2)]}),
            pages: BTreeMap::new(),
            apps: Some(
                json!({"selected":selected,"apps":[app],"ungrouped_members":0,
                "page":{"offset":0,"total":1,"next_offset":null}}),
            ),
            detail: Some(
                json!({"selected":selected,"app":app,"elapsed_ns":"1000000000",
                "processes":self.processes(),
                "page":{"offset":0,"total":app["members"],"next_offset":null}}),
            ),
            history: Some(json!({"selected":wire_observation(second,publication),
                "context_seconds":900,"points":points})),
            cpu_detail: None,
            cpu_history: None,
            resident_history: None,
            app_attribution_history: None,
            overview_app_histories: None,
            process_history: None,
            rankings: None,
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
        "disk_read_bps":null,"disk_written_bps":null,"disk_io_bps":null})
}
