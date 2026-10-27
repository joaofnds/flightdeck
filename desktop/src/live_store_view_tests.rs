use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::json;
use viewer::apps::Stamp;
use viewer::live_app_response::{App, Key, KeyKind, Measure, Observation, RankedApp};
use viewer::store::{AppRow, AppsSecond, MachineSecond, Totals, View};

use super::super::LiveDashboard;
use crate::data::{Line, Panel};
use crate::live_app_attribution_history::{AppAttributionHistory, Metric};
use crate::live_hover::{Origin, Preview, Target};
use crate::live_overview_app_histories::OverviewAppHistories;
use crate::live_projection::overview_panels;
use crate::live_resident_history::ResidentHistory;
use crate::live_source::{Update, Worker};
use crate::live_store_fixtures::{
    levels, levels_named, reading_at, store_levels_named, trend_point, view,
};
use crate::live_store_page_fixtures::{
    figure_readings, figure_second, readings, summary_selection, texts,
};
use crate::live_views::summary;
use crate::live_views::utilization::UtilizationReadings;

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
fn pause_keeps_the_displayed_publication_among_same_second_siblings() {
    // Goal: Pause holds the displayed publication, not another reading in the same wall second.
    // Method: show distinct sibling readings and inspect the exact target sent by Pause.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    let first = reading_at(10_005, 0);
    let shown = MachineSecond {
        publication: 10_006,
        wall_ns: first.wall_ns + 500_000_000,
        cpu_percent: Some(91.0),
        ..first.clone()
    };
    let mut view = view(1, &[], &[first.clone(), shown.clone()]);
    view.apps[0] = Arc::new(apps_of(&first, "Browser", 100_000_000));
    view.apps[1] = Arc::new(apps_of(&shown, "Browser", 900_000_000));
    dashboard.take_view(Arc::new(view));

    let held = dashboard.pause_target();

    assert_eq!(held.0, Some(10_005));
    assert_eq!(held.1.as_ref().unwrap()["publication"], 10_006);
}

#[test]
fn a_retained_hover_does_not_substitute_a_different_publication() {
    // Goal: a stale exact target cannot read a different publication of the same wall second.
    // Method: retain only the newer sibling, then ask the hover adapter for the older identity.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    let old = reading_at(8_200, 0);
    let newer = MachineSecond {
        publication: 8_201,
        ..old.clone()
    };
    let mut held = view(1, std::slice::from_ref(&newer), &[reading_at(10_005, 0)]);
    let budget = Arc::new(viewer::store::ByteBudget::new(1_048_576));
    held.sampled
        .push(Arc::new(viewer::store::SampledSecond::from_second(
            &newer,
            &apps_of(&newer, "Newer", 20),
            &budget,
        )));
    dashboard.take_view(Arc::new(held));
    let target = Target {
        second: 8_200,
        identity: Some(crate::live_rankings::stamp_identity(
            &apps_of(&old, "Older", 10).observation.unwrap().stamp,
        )),
    };

    let reading = dashboard.store_view_held(&target);

    assert!(reading.and_then(|held| held.second).is_none());
}

#[test]
fn a_recent_hover_finds_the_exact_sibling_instead_of_the_last_in_its_second() {
    // Goal: an exact recent target reads its own publication even when a newer sibling exists.
    // Method: retain distinct siblings and request the first one's identity through the adapter.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    let old = reading_at(10_005, 0);
    let newer = MachineSecond {
        publication: 10_006,
        cpu_percent: Some(91.0),
        ..old.clone()
    };
    let old_apps = apps_of(&old, "Older", 10);
    let target = Target {
        second: 10_005,
        identity: Some(crate::live_rankings::stamp_identity(
            &old_apps.observation.unwrap().stamp,
        )),
    };
    let mut held = view(1, &[], &[old.clone(), newer.clone()]);
    held.apps = vec![Arc::new(old_apps), Arc::new(apps_of(&newer, "Newer", 20))];
    dashboard.take_view(Arc::new(held));

    let reading = dashboard.store_view_held(&target).unwrap();

    assert_eq!(reading.second.as_deref(), Some(&old));
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
fn a_view_newer_than_the_readings_fills_the_utilization_panel() {
    // Goal: Overview's utilization panel shows the newest second the store holds, as its
    // readouts do, so it keeps following the Mac while the worker reads nothing. Method:
    // readings that end at 10,000, then a View that ends at 10,005, whose panel reads as the
    // worker's summary of 10,005 reads it.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);

    dashboard.take_view(newer_view());

    let expected = UtilizationReadings::of_summary(&trend_point(&reading_at(10_005, 0)));
    assert_eq!(dashboard.overview_utilization(), expected);
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

#[test]
fn a_view_newer_than_the_readings_draws_the_overviews_charts_by_app() {
    // Goal: Overview, following the newest second, draws its CPU, memory, energy and disk
    // charts by App from the store's View, under the titles Overview gives them. Method:
    // readings that end at 10,000 and draw no chart by App, then a View that ends five seconds
    // later and holds an App in each second.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    let view = Arc::new(charted_view());

    dashboard.take_view(Arc::clone(&view));

    assert_eq!(by_app(&dashboard.panels), expected_by_app(&view));
}

#[test]
fn a_view_newer_than_the_readings_reads_each_chart_by_app_at_its_newest_second() {
    // Goal: a chart by App drawn from the View reads its newest second as the worker's chart of
    // the same seconds reads it. Method: readings that end at 10,000, then a View that ends five
    // seconds later and holds an App in each second, beside a page showing the worker's reading
    // and charts by App of those seconds.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);

    dashboard.take_view(Arc::new(charted_view()));

    let worker = drawn_by_app(&newer_seconds());
    for id in [
        crate::live_resident_history::PANEL_ID,
        Metric::Cpu.panel_id(),
        Metric::Power.panel_id(),
        Metric::Disk.panel_id(),
    ] {
        assert_eq!(chart_reading(&dashboard, id), chart_reading(&worker, id));
    }
}

mod when_the_view_cannot_choose_the_apps_its_charts_draw {
    use super::*;

    #[test]
    fn the_page_draws_no_chart_by_app_and_says_why() {
        // Goal: where the store could not choose the Apps its charts draw, Overview draws none
        // of its charts by App, not even the worker's older ones, and their cards say why.
        // Method: readings with charts by App, then a newer View whose cohorts the store refused.
        let mut dashboard = drawn_by_app(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        let mut view = charted_view();
        view.cohorts.refused = true;

        dashboard.take_view(Arc::new(view));

        let cpu = dashboard.attribution_history(Metric::Cpu).unwrap();
        let resident = dashboard.resident_history().unwrap();
        assert_eq!(by_app(&dashboard.panels), Vec::new());
        assert_eq!(
            cpu.coverage(None),
            "App rate history unavailable: App rate history holds more Apps than it adds up"
        );
        assert_eq!(
            resident.coverage(None),
            "Resident App history unavailable: Resident App history holds more Apps than it adds up"
        );
    }
}

mod when_the_worker_has_not_read_the_page_yet {
    use super::*;

    #[test]
    fn the_view_draws_the_overviews_charts_by_app() {
        // Goal: at launch Overview draws its charts by App from the store's View before the
        // worker's first reading. Method: a newly opened Overview, then a View that holds an App
        // in each second.
        let mut dashboard = opening();
        let view = Arc::new(charted_view());

        dashboard.take_view(Arc::clone(&view));

        assert_eq!(by_app(&dashboard.panels), expected_by_app(&view));
    }

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
        hover.reading.selection =
            crate::live_source::Presentation::Legacy(summary_selection(&reading_at(9_999, 0)));
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
        hover.reading.selection =
            crate::live_source::Presentation::Legacy(summary_selection(&reading_at(9_999, 0)));
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
    snapshot.selection.legacy_mut().unwrap()["summary"] = trend_point(seconds.last().unwrap());

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

/// The View of `newer_seconds`, in each of which one App, Brave Browser, spent 0.75 s of CPU,
/// and whose charts by App draw it on a line of its own.
fn charted_view() -> View {
    let seconds = newer_seconds();
    let mut view = view(1, &[], &seconds);
    view.apps = seconds
        .iter()
        .map(|second| Arc::new(apps_of(second, "Brave Browser", 750_000_000)))
        .collect();
    let brave = Arc::new(RankedApp::named(
        KeyKind::Bundle,
        b"Brave Browser".to_vec(),
        "Brave Browser".into(),
    ));
    view.cohorts.seconds = seconds.len();
    view.cohorts.cpu = vec![Arc::clone(&brave)];
    view.cohorts.power = vec![Arc::clone(&brave)];
    view.cohorts.disk = vec![Arc::clone(&brave)];
    view.cohorts.resident = vec![brave];
    view
}

/// An Overview showing the worker's readings of `trends` with the charts by App the worker
/// drew of them.
fn drawn_by_app(trends: &[MachineSecond]) -> LiveDashboard {
    let charted = charted_view();
    let history = |metric| {
        Arc::new(crate::live_source::Presentation::Legacy(
            AppAttributionHistory::from_view(metric, &charted),
        ))
    };
    let mut snapshot = readings(trends);
    snapshot.overview_app_histories = Some(Arc::new(OverviewAppHistories {
        cpu: history(Metric::Cpu),
        power: history(Metric::Power),
        disk: history(Metric::Disk),
    }));
    snapshot.resident_history = Some(std::sync::Arc::new(
        crate::live_source::Presentation::Shared(Arc::new(ResidentHistory::from_view(&charted))),
    ));

    let mut dashboard = opening();
    dashboard.accept(snapshot);
    assert!(dashboard.error.is_none());
    assert_eq!(by_app(&dashboard.panels).len(), 4);
    dashboard
}

/// The id, title and lines of each of `panels` that Overview's CPU, memory, energy and disk
/// charts by App draw, in order.
fn by_app(panels: &[Rc<Panel>]) -> Vec<(String, String, Vec<Line>)> {
    let ids = [
        crate::live_resident_history::PANEL_ID,
        Metric::Cpu.panel_id(),
        Metric::Power.panel_id(),
        Metric::Disk.panel_id(),
    ];

    panels
        .iter()
        .filter(|panel| ids.contains(&panel.id.as_str()))
        .map(|panel| (panel.id.clone(), panel.title.clone(), panel.lines.clone()))
        .collect()
}

/// What `by_app` finds of Overview's charts by App drawn from `view`.
fn expected_by_app(view: &View) -> Vec<(String, String, Vec<Line>)> {
    let titled = [
        (Metric::Cpu, "CPU by app"),
        (Metric::Power, "Energy by app"),
        (Metric::Disk, "Disk by app"),
    ];
    let resident = ResidentHistory::from_view(view).panel().unwrap();

    let mut expected = vec![(
        resident.id.clone(),
        "Memory by app".to_owned(),
        resident.lines.clone(),
    )];
    expected.extend(titled.into_iter().map(|(metric, title)| {
        let panel = AppAttributionHistory::from_view(metric, view)
            .panel()
            .unwrap();
        (panel.id.clone(), title.into(), panel.lines.clone())
    }));

    expected
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
    let ranked = Arc::new(RankedApp::named(
        KeyKind::Bundle,
        app.key.bytes.clone(),
        app.display_name.clone(),
    ));
    let apps = vec![AppRow::new(&app, ranked)];

    AppsSecond {
        metadata: viewer::store::AppMetadata::default(),
        processes: Err(viewer::store::Missing::Unread),
        missing: None,
        observation: Some(Observation {
            recording: [1, 2],
            stamp: Stamp {
                boot: [3, 4],
                run: [1, 2],
                publication: second.publication,
                continuous_ns: second.continuous_ns,
                awake_ns: second.awake_ns,
            },
            run_tick: second.run_tick,
            wall_ns: Some(second.wall_ns),
        }),
        consecutive: true,
        elapsed_ns: Some(1_000_000_000),
        membership_complete: true,
        ungrouped_members: 0,
        live_ungrouped_members: 0,
        usage_refused: 0,
        usage_gone: 0,
        totals: Totals::of(&apps),
        apps: apps.try_into().unwrap(),
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
        held: None,
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

fn assert_same_panels(panels: &[Rc<Panel>], expected: &[Rc<Panel>]) {
    assert_eq!(panels.len(), expected.len());
    for (panel, expected) in panels.iter().zip(expected) {
        assert!(Rc::ptr_eq(panel, expected));
    }
}

#[gpui::test]
fn hidden_window_pauses_the_store_and_showing_it_resumes_ingestion(cx: &mut gpui::TestAppContext) {
    // Goal: native visibility controls store work as well as the legacy page reader.
    // Method: hide and show a real window over a registered source and observe store parking.
    use gpui::{AppContext, WindowOptions, WindowVisibility};
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let store = viewer::store::Store::start(&recording.root).unwrap();
    let worker = Worker::start(recording.root.clone()).unwrap();
    cx.update(|cx| super::install_store(store, cx));
    let window = cx.update(|cx| {
        cx.open_window(WindowOptions::default(), |window, cx| {
            let dashboard = cx.new(|_| LiveDashboard::new(worker, "overview".into(), None));
            crate::live_dashboard::observe_visibility(&dashboard, window, cx);
            dashboard
        })
        .unwrap()
    });

    cx.simulate_window_visibility_change(window.into(), WindowVisibility::Hidden);
    wait_for_store_activity(cx, true);
    recording.publish();
    cx.simulate_window_visibility_change(window.into(), WindowVisibility::Visible);
    wait_for_store_activity(cx, false);

    assert!(!cx.update(|cx| cx.global::<super::LiveStore>().0.is_paused()));
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

#[gpui::test]
fn showing_a_pinned_window_keeps_the_store_paused_until_live_is_selected(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: restoring a pinned selection never starts live ingestion in the store.
    // Method: show a pinned window over a paused real store, then explicitly return to live.
    use gpui::{AppContext, WindowOptions, WindowVisibility};
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let store = viewer::store::Store::start(&recording.root).unwrap();
    let worker = Worker::start(recording.root.clone()).unwrap();
    cx.update(|cx| super::install_store(store, cx));
    let window = cx.update(|cx| {
        cx.open_window(WindowOptions::default(), |window, cx| {
            let dashboard = cx.new(|cx| {
                let mut dashboard = LiveDashboard::new(worker, "overview".into(), None);
                dashboard.pin_second(1000, cx);
                dashboard
            });
            crate::live_dashboard::observe_visibility(&dashboard, window, cx);
            dashboard
        })
        .unwrap()
    });
    cx.simulate_window_visibility_change(window.into(), WindowVisibility::Hidden);
    wait_for_store_activity(cx, true);

    cx.simulate_window_visibility_change(window.into(), WindowVisibility::Visible);
    window
        .update(cx, |dashboard, _, cx| dashboard.tick(cx))
        .unwrap();

    assert!(cx.update(|cx| cx.global::<super::LiveStore>().0.is_paused()));
    assert!(!cx.update(|cx| cx.global::<super::LiveStore>().0.is_active()));
    window
        .update(cx, |dashboard, _, cx| {
            dashboard.reset_range(cx);
            dashboard.tick(cx);
        })
        .unwrap();
    wait_for_store_activity(cx, false);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

fn wait_for_store_activity(cx: &mut gpui::TestAppContext, paused: bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    for _ in 0..2_000 {
        cx.run_until_parked();
        if cx.update(|cx| cx.global::<super::LiveStore>().0.is_paused()) == paused {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "store activity did not change"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("store activity did not change");
}

#[gpui::test]
fn a_live_page_other_than_overview_parks_the_unused_store(cx: &mut gpui::TestAppContext) {
    // Goal: a page with its own reader cannot also spend CPU refreshing an unused Store View.
    // Method: tick CPU with an installed live store, then navigate to Overview and tick again.
    use gpui::AppContext;
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let store = viewer::store::Store::start(&recording.root).unwrap();
    cx.update(|cx| super::install_store(store, cx));
    let page = cx.update(|cx| cx.new(|_| opening_on("cpu")));

    page.update(cx, LiveDashboard::tick);

    wait_for_store_activity(cx, true);
    assert!(!cx.update(|cx| cx.global::<super::LiveStore>().0.is_active()));
    page.update(cx, |page, cx| {
        page.navigate("overview", cx);
        page.tick(cx);
    });
    wait_for_store_activity(cx, false);
    assert!(cx.update(|cx| cx.global::<super::LiveStore>().0.is_active()));
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

#[gpui::test]
fn a_resting_overview_polls_the_installed_store_until_its_view_arrives(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: the real store's pending handoff reaches Overview's fast-poll decision.
    // Method: hold ingestion paused, tick a rested page, then resume and accept the new View.
    use gpui::AppContext;
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let store = viewer::store::Store::start(&recording.root).unwrap();
    store.set_active(false);
    cx.update(|cx| super::install_store(store, cx));
    wait_for_store_activity(cx, true);
    let stale = MachineSecond {
        publication: 1,
        ..reading_at(1000, 0)
    };
    let page = cx.update(|cx| {
        cx.new(|_| {
            let mut page = opening();
            let mut snapshot = readings(std::slice::from_ref(&stale));
            snapshot.generation = page.generation;
            assert!(page.receive(Update::Ready(snapshot)));
            assert!(page.take_view(Arc::new(view(1, &[], &[stale]))));
            page.tick_rest();
            assert!(page.reader_rested());
            page
        })
    });

    page.update(cx, LiveDashboard::tick);
    assert!(cx.update(|cx| page.read(cx).awaiting_reply()));
    recording.publish();
    cx.update(|cx| super::set_store_active(true, cx));
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    for _ in 0..2_000 {
        page.update(cx, LiveDashboard::tick);
        let delivered = cx.update(|cx| {
            page.read(cx).store_view.as_ref().is_some_and(|view| {
                view.seconds
                    .last()
                    .is_some_and(|second| second.publication == 2)
            })
        });
        if delivered {
            if !cx.update(|cx| page.read(cx).awaiting_reply()) {
                break;
            }
        }

        assert!(
            std::time::Instant::now() < deadline,
            "View handoff never finished"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let delivered = cx.update(|cx| {
        page.read(cx)
            .store_view
            .as_ref()
            .and_then(|view| view.seconds.last())
            .map(|second| second.publication)
    });
    assert_eq!(delivered, Some(2));
    assert!(!cx.update(|cx| page.read(cx).awaiting_reply()));
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}
