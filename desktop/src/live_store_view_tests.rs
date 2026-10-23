use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use collector::machine::Pressure;
use serde_json::json;
use viewer::apps::Stamp;
use viewer::live_app_response::{App, Key, KeyKind, Measure, Observation, RankedApp};
use viewer::store::{AppRow, AppsSecond, BatterySecond, MachineSecond, MemorySecond, Totals, View};

use super::super::LiveDashboard;
use crate::data::Panel;
use crate::hardware::Readings;
use crate::live_hover::{Origin, Preview, Target};
use crate::live_projection::overview_panels;
use crate::live_source::{Snapshot, Update, Worker};
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
fn pause_holds_the_views_newest_second() {
    // Goal: Pause holds the second the page shows, which the View holds and the worker has not
    // read yet. Method: readings that end at 10,000, a View that ends at 10,005, then the
    // second Pause would hold.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    dashboard.take_view(newer_view());

    let held = dashboard.pause_target();

    assert_eq!(held, (Some(10_005), None));
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
fn a_view_newer_than_the_readings_fills_the_hardware_figure() {
    // Goal: the hardware figure shows the newest second the store holds, as the charts and
    // readouts do. Method: readings that end at 10,000, then a View whose newest second, at
    // 10,005, holds the figure's readings.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    let view = view(1, &[], &[reading_at(10_000, 0), figure_second(10_005)]);

    dashboard.take_view(Arc::new(view));

    assert_eq!(
        dashboard.live_hardware_readings(),
        (figure_readings(), [None, None])
    );
}

#[test]
fn a_view_newer_than_the_readings_reads_each_chart_at_its_newest_second() {
    // Goal: a chart drawn from the View names the View's newest second and reads its value
    // there, as the worker's reading of the same seconds would. Method: readings that end at
    // 10,000, then a View that ends at 10,005, whose CPU and memory pressure charts read as a
    // page showing the worker's reading of 10,005 reads them.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);

    dashboard.take_view(newer_view());

    let worker = read_by_the_worker(&newer_seconds());
    assert_eq!(
        chart_reading(&dashboard, "live-cpu"),
        chart_reading(&worker, "live-cpu")
    );
    assert_eq!(
        chart_reading(&dashboard, "live-memory-pressure"),
        chart_reading(&worker, "live-memory-pressure")
    );
}

#[test]
fn the_status_judges_the_views_newest_second() {
    // Goal: the page reads live while the View it draws keeps up with the clock, though the
    // worker's newest reading is older. Method: readings that end at 10,000, a View that ends
    // at 10,005, and the clock at 10,006.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);

    dashboard.take_view(newer_view());

    assert_eq!(
        dashboard.status_at(UNIX_EPOCH + Duration::from_secs(10_006)),
        "Live"
    );
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

#[test]
fn a_view_newer_than_the_readings_ranks_the_apps_of_its_newest_second() {
    // Goal: Overview's top Apps show the newest second the store holds, as its charts do.
    // Method: readings that end at 10,000 and rank no Apps, then a View whose newest second, at
    // 10,005, holds one App that spent 0.75 s of CPU.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);

    dashboard.take_view(ranked_view());

    let (rankings, target) = dashboard.ranking_reading().unwrap();
    let cpu = &rankings.metrics[0].entries[0];
    assert_eq!(target.second, 10_005);
    assert_eq!(cpu.app.name, "Brave Browser");
    assert_eq!(cpu.amount, 750_000_000);
}

mod when_the_worker_has_not_read_the_page_yet {
    use super::*;

    #[test]
    fn the_view_draws_the_overviews_machine_charts() {
        // Goal: a window opened at launch draws Overview's machine charts from the store's View,
        // which comes seconds before the worker's first reading. Method: a newly opened
        // Overview the worker has not answered, then a View.
        let mut dashboard = opening();
        let view = newer_view();

        let changed = dashboard.take_view(Arc::clone(&view));

        assert!(changed);
        for panel in overview_panels(&view, levels().as_ref()).unwrap() {
            assert_eq!(drawn(&dashboard, &panel.id).lines, panel.lines);
        }
    }

    #[test]
    fn the_view_ranks_the_apps_of_its_newest_second() {
        // Goal: a window opened at launch shows the top Apps of the View's newest second rather
        // than wait for the worker. Method: a newly opened Overview, then a View whose newest
        // second, at 10,005, holds one App.
        let mut dashboard = opening();

        dashboard.take_view(ranked_view());

        let (rankings, target) = dashboard.ranking_reading().unwrap();
        assert_eq!(target.second, 10_005);
        assert_eq!(rankings.metrics[0].entries[0].app.name, "Brave Browser");
    }

    #[test]
    fn the_view_fills_the_overviews_readouts() {
        // Goal: the readouts of a window opened at launch show the View's newest second rather
        // than wait for the worker. Method: a newly opened Overview, then a View that ends at
        // 10,005, whose readouts read as the worker's reading of 10,005 reads them.
        let mut dashboard = opening();

        dashboard.take_view(newer_view());

        let expected = summary::cards("overview", &summary_selection(&reading_at(10_005, 0)));
        assert_eq!(texts(&dashboard.summary_cards()), texts(&expected));
    }

    #[test]
    fn the_view_fills_the_hardware_figure() {
        // Goal: the hardware figure of a window opened at launch shows the View's newest
        // second. Method: a newly opened Overview, then a View whose newest second holds the
        // figure's readings.
        let mut dashboard = opening();
        let view = view(1, &[], &[reading_at(10_000, 0), figure_second(10_005)]);

        dashboard.take_view(Arc::new(view));

        assert_eq!(
            dashboard.live_hardware_readings(),
            (figure_readings(), [None, None])
        );
    }

    #[test]
    fn each_chart_reads_the_views_newest_second() {
        // Goal: a chart drawn from the View at launch reads the View's newest second rather
        // than wait for the worker. Method: a newly opened Overview, then a View that ends at
        // 10,005, whose CPU and memory pressure charts read as a page showing the worker's
        // reading of 10,005 reads them.
        let mut dashboard = opening();

        dashboard.take_view(newer_view());

        let worker = read_by_the_worker(&newer_seconds());
        assert_eq!(
            chart_reading(&dashboard, "live-cpu"),
            chart_reading(&worker, "live-cpu")
        );
        assert_eq!(
            chart_reading(&dashboard, "live-memory-pressure"),
            chart_reading(&worker, "live-memory-pressure")
        );
    }

    #[test]
    fn the_status_reads_live() {
        // Goal: a window opened at launch reads live once the View draws it, rather than
        // reporting no readings until the worker answers. Method: a newly opened Overview, a
        // View that ends at 10,005, and the clock at 10,006.
        let mut dashboard = opening();

        dashboard.take_view(newer_view());

        assert_eq!(
            dashboard.status_at(UNIX_EPOCH + Duration::from_secs(10_006)),
            "Live"
        );
    }
}

mod when_the_views_newest_second_holds_no_apps {
    use super::*;

    #[test]
    fn the_top_apps_say_why() {
        // Goal: top Apps the store has not read say so, rather than show an older second's Apps
        // beside the charts of a newer one. Method: a View whose newest second holds no Apps,
        // as a second read from a sealed minute holds none.
        let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);

        dashboard.take_view(newer_view());

        let error = dashboard.ranking_reading().err();
        let expected = "App totals unavailable: this second's Apps are not read yet";
        assert_eq!(error.as_deref(), Some(expected));
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

mod when_the_window_reads_a_saved_recording {
    use super::*;

    #[test]
    fn the_worker_s_readings_draw_the_charts() {
        // Goal: the store follows the live Mac, so a window that opened a saved recording draws
        // the recording's readings and never the live View under the recording's name. Method:
        // Overview of a recording, then a newer View.
        let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        dashboard.native.source = crate::live_source_mode::SourceMode::Recorded;
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

mod when_the_view_cannot_be_drawn {
    use super::*;

    #[test]
    fn the_page_shows_the_worker_s_readings_and_no_failure() {
        // Goal: a View the page cannot draw leaves the worker's readings whole on the page, and
        // the page reports no failure of its own reading. Method: readings that end at 10,000,
        // then a newer View whose newest power reading is no number.
        let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        let mut seconds = newer_seconds();
        seconds[2].power_watts = Some(f32::NAN);

        let changed = dashboard.take_view(Arc::new(view(1, &[], &seconds)));

        assert!(!changed);
        assert!(dashboard.error.is_none());
        assert_eq!(dashboard.shown_second(), Some(10_000));
    }
}

mod when_the_page_keeps_the_room_of_its_readings {
    use super::*;

    #[test]
    fn the_view_draws_nothing_until_the_reply() {
        // Goal: a page asked for again keeps the room its readings took until the worker
        // answers, and a View that arrives meanwhile draws no chart into it. Method: Overview
        // with readings asks again, as paging a table does, then a newer View.
        let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        dashboard.submit();

        let changed = dashboard.take_view(newer_view());

        assert!(!changed);
    }
}

mod when_the_worker_fails_to_read_the_page {
    use super::*;

    #[test]
    fn the_status_reports_the_failure() {
        // Goal: a failed read shows as one while the store's Views keep arriving. Method: a
        // newly opened Overview whose read fails, then a newer View.
        let mut dashboard = opening();
        let generation = dashboard.generation;
        let message = String::from("unreadable");
        let failure = Update::Failed {
            generation,
            message,
        };
        assert!(dashboard.receive(failure));

        dashboard.take_view(newer_view());

        let now = UNIX_EPOCH + Duration::from_secs(10_006);
        assert_eq!(dashboard.status_at(now), "Reading unavailable");
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

    #[test]
    fn the_hardware_figure_shows_the_hovered_second() {
        // Goal: the figure reads the second under the pointer, as the readouts do, rather than
        // the View's newest. Method: a hover's reading of 9,999, whose CPU was 99.5% busy, then
        // a View that ends at 10,005, whose CPU was 2.5% busy.
        let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        let mut hover = preview(&dashboard);
        hover.reading.selection = summary_selection(&reading_at(9_999, 0));
        dashboard.preview = Some(hover);

        dashboard.take_view(newer_view());

        let (readings, failures) = dashboard.live_hardware_readings();
        assert_eq!(failures, [None, None]);
        assert_eq!(readings.utilization[0], Some(99.5));
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
    let mut dashboard = opening_on(route);
    dashboard.accept(readings(trends));
    assert!(dashboard.error.is_none());
    dashboard
}

/// An Overview newly opened, which the worker has not answered yet, as at launch.
fn opening() -> LiveDashboard {
    opening_on("overview")
}

/// The page `route` newly opened, which the worker has not answered yet.
fn opening_on(route: &str) -> LiveDashboard {
    let root = std::env::temp_dir().join(format!(
        "flightdeck-missing-store-view-{}",
        std::process::id()
    ));
    let dashboard = LiveDashboard::new(Worker::start(root).unwrap(), route.into(), None);
    assert!(dashboard.is_opening());
    dashboard
}

/// An Overview showing the worker's readings of `seconds`, with the summary of the newest, as
/// the worker reads them.
fn read_by_the_worker(seconds: &[MachineSecond]) -> LiveDashboard {
    let mut snapshot = readings(seconds);
    snapshot.selection["summary"] = trend_point(seconds.last().unwrap());

    let mut dashboard = opening();
    dashboard.accept(snapshot);
    assert!(dashboard.error.is_none());
    dashboard
}

/// What the chart `id` reads on the page, as macOS's accessibility reads it: its second and
/// its reading there.
fn chart_reading(dashboard: &LiveDashboard, id: &str) -> (Option<u64>, String) {
    dashboard.chart_accessibility_reading(drawn(dashboard, id))
}

/// The View of `newer_seconds`.
fn newer_view() -> Arc<View> {
    Arc::new(view(1, &[], &newer_seconds()))
}

/// The View of `newer_seconds`, whose newest second holds one App, Brave Browser, that spent
/// 0.75 s of CPU in it.
fn ranked_view() -> Arc<View> {
    let seconds = newer_seconds();
    let mut view = view(1, &[], &seconds);
    view.apps[2] = Arc::new(apps_of(&seconds[2], "Brave Browser", 750_000_000));
    Arc::new(view)
}

/// The Apps of `second` when one bundle, named `name`, is all that ran, and spent `cpu_ns` of
/// CPU time in the second that ends there and nothing else.
pub fn apps_of(second: &MachineSecond, name: &str, cpu_ns: u64) -> AppsSecond {
    let measure = |known| Measure {
        known,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    };
    let app = App {
        key: Key {
            kind: KeyKind::Bundle,
            bytes: name.as_bytes().to_vec(),
            display: name.into(),
        },
        display_name: name.into(),
        members: 1,
        live_members: 1,
        membership_complete: true,
        cpu_ns: measure(cpu_ns),
        energy_nj: measure(0),
        gpu_ns: measure(0),
        disk_read_bytes: measure(0),
        disk_written_bytes: measure(0),
        disk_io_bytes: measure(0),
        resident_bytes: measure(0),
        footprint_bytes: measure(0),
    };
    let ranked = Arc::new(RankedApp {
        kind: KeyKind::Bundle,
        bytes: app.key.bytes.clone(),
        display_name: app.display_name.clone(),
    });
    let apps = vec![AppRow::new(&app, ranked)];

    AppsSecond {
        missing: None,
        observation: Some(Observation {
            recording: [1, 2],
            stamp: Stamp {
                boot: [3, 4],
                run: [1, 2],
                publication: second.publication,
                continuous_ns: second.continuous_ns,
                awake_ns: None,
            },
            run_tick: second.run_tick,
            wall_ns: Some(second.wall_ns),
        }),
        elapsed_ns: Some(1_000_000_000),
        membership_complete: true,
        ungrouped_members: 0,
        usage_refused: 0,
        usage_gone: 0,
        totals: Totals::of(&apps),
        apps,
    }
}

/// The seconds 9,999, 10,000 and 10,005, five seconds newer than the worker's readings.
fn newer_seconds() -> [MachineSecond; 3] {
    [
        reading_at(9_999, 0),
        reading_at(10_000, 0),
        reading_at(10_005, 0),
    ]
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

/// The second at `wall_second` whose hardware figure shows CPU and GPU dies at 61.5 and 52.25 °C,
/// fans at 1,200 and 1,500 rpm, the CPU 25% and the GPU 12.5% busy, 12 of 32 GiB in use, the
/// battery 75% charged and giving 6.5 W, and moderate thermal pressure.
fn figure_second(wall_second: u64) -> MachineSecond {
    const GIBIBYTE: u64 = 1_073_741_824;

    MachineSecond {
        cpu_celsius: Some(61.5),
        gpu_celsius: Some(52.25),
        fan_rpm: [Some(1_200.0), Some(1_500.0)],
        cpu_percent: Some(25.0),
        gpu_active_percent: Some(12.5),
        memory: Some(MemorySecond {
            total_bytes: 32 * GIBIBYTE,
            app_bytes: Some(8 * GIBIBYTE),
            wired_bytes: 2 * GIBIBYTE,
            compressed_bytes: 2 * GIBIBYTE,
            pressure: Pressure::Normal,
        }),
        battery: Some(BatterySecond {
            charge_percent: Some(75.0),
            power_watts: Some(-6.5),
        }),
        thermal_pressure: Some(1),
        ..reading_at(wall_second, 0)
    }
}

/// The hardware figure's readings of `figure_second`.
fn figure_readings() -> Readings {
    Readings {
        temperatures: [Some(61.5), Some(52.25), None],
        speeds: [Some(1_200.0), Some(1_500.0)],
        utilization: [Some(25.0), Some(12.5)],
        memory: [Some(12.0), Some(32.0)],
        battery: [Some(75.0), Some(-6.5)],
        thermal: "Moderate".into(),
    }
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
