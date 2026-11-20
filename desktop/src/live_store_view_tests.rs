use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};
use viewer::apps::Stamp;
use viewer::live_app_response::{App, Key, KeyKind, Measure, Observation, RankedApp};
use viewer::store::{AppRow, AppsSecond, BatterySecond, Cohorts, MachineSecond, Totals, View};

use super::super::LiveDashboard;
use crate::data::{Line, Panel};
use crate::live_app_attribution_history::{AppAttributionHistory, Metric};
use crate::live_hover::{Origin, Preview, Target};
use crate::live_overview_app_histories::OverviewAppHistories;
use crate::live_resident_history::ResidentHistory;
use crate::live_source::{Update, Worker};
use crate::live_store_app_fixtures::apps_at;
use crate::live_store_fixtures::overview_panels;
use crate::live_store_fixtures::{
    levels, levels_named, reading_at, store_levels_named, trend_point, view, with_memory_reading,
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
    view.apps
        .replace(0, Arc::new(apps_of(&first, "Browser", 100_000_000)));
    view.apps
        .replace(1, Arc::new(apps_of(&shown, "Browser", 900_000_000)));
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
    held.apps = [Arc::new(old_apps), Arc::new(apps_of(&newer, "Newer", 20))]
        .into_iter()
        .collect();
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

    let expected = summary::cards(
        "overview",
        &crate::live_store_fixtures::selection_of_json(&summary_selection(&reading_at(10_005, 0))),
    );
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
fn a_rested_overview_reads_the_cooling_card_of_the_views_newest_second() {
    // Goal: once Overview's reader rests, its "Cooling and battery" card goes on showing the
    // newest second the store holds, as its charts do. Method: the worker's reading of 10,000,
    // a View of the same seconds that the page rests its reader on, then a View that ends at
    // 10,005 with another SoC temperature, fan speeds and charge, whose card reads as the
    // worker's reading of 10,005 reads it.
    let first = [cooled_at(9_999), cooled_at(10_000)];
    let newer = [cooled_at(9_999), cooled_at(10_000), cooled_at(10_005)];
    let mut dashboard = opening();
    let mut reading = worker_reading(&first);
    reading.generation = dashboard.generation;
    assert!(dashboard.receive(Update::Ready(reading)));
    dashboard.take_view(Arc::new(view(1, &[], &first)));
    dashboard.tick_rest();
    assert!(dashboard.reader_rested());

    dashboard.take_view(Arc::new(view(2, &[], &newer)));

    let expected = read_by_the_worker(&newer).overview_cooling_readings();
    assert_ne!(
        read_by_the_worker(&first).overview_cooling_readings(),
        expected
    );
    assert_eq!(dashboard.overview_cooling_readings(), expected);
}

#[test]
fn a_rested_memory_page_shows_the_memory_of_a_newest_second_without_a_complete_reading() {
    // Goal: a Memory page whose reader rests shows the memory the View records for its newest
    // second where the store holds no complete reading of that second, as where it refused one,
    // since the resting reader never reads that second. Method: the worker reads memory seconds
    // 9,999 and 10,000, the View of them rests the reader, then a View adds 10,001 with the
    // memory of its record alone, and the memory the page's readings show.
    let first = memory_seconds(&[9_999, 10_000]);
    let mut dashboard = rested_on("memory", &first, worker_reading(&first));
    let newest = reading_at(10_001, 0);
    let memory = newest.memory.unwrap();
    let seconds = [first[0].clone(), first[1].clone(), newest];

    assert!(dashboard.take_view(Arc::new(view(2, &[], &seconds))));

    let shown = dashboard.shown_summary();
    assert_eq!(dashboard.shown_second(), Some(10_001));
    assert_eq!(
        summary::memory_in_use_bytes(&shown),
        summary::memory_in_use_bytes_of(
            memory.app_bytes.unwrap(),
            memory.wired_bytes,
            memory.compressed_bytes
        )
    );
    assert_eq!(
        shown["memory_total_bytes"].as_u64(),
        Some(memory.total_bytes)
    );
    assert_eq!(
        shown["swap_used_bytes"].as_u64(),
        Some(memory.swap_used_bytes)
    );
    assert_eq!(
        shown["swap_total_bytes"].as_u64(),
        Some(memory.swap_total_bytes)
    );
}

#[test]
fn every_page_drawn_from_the_view_draws_its_machine_charts_from_it() {
    // Goal: the pages drawn from the store's View are the pages the View has machine charts of,
    // so a page named in one list and not the other fails here rather than on the user's Mac.
    // Method: draw every route's machine charts from a View, then whether each draws charts
    // against whether the page is drawn from the View.
    let view = view(1, &[], &memory_seconds(&[9_999, 10_000]));

    for (route, _) in crate::live_dashboard::ROUTES {
        let panels = crate::live_projection::view_panels(route, &view, levels().as_ref());

        let drawn = panels.is_ok_and(|panels| !panels.is_empty());
        assert_eq!(drawn, super::store_view_draws_machine(route), "{route}");
    }
}

#[test]
fn a_rested_overview_shows_the_record_of_a_refused_second_the_worker_read() {
    // Goal: Overview shows what the store's record of a second holds where the store refused
    // that second's complete reading, even where the worker read the same second, whose point
    // then holds none of the readings. Method: the worker reads 10,000 and a refused 10,001 and
    // writes the point it writes for a refused second, the View of them rests the reader, then
    // the readings of the shown second against the record's.
    assert_shows_the_record_of_a_refused_second("overview");
}

#[test]
fn a_rested_memory_page_shows_the_record_of_a_refused_second_the_worker_read() {
    // Goal and method: those of the Overview test above, on the Memory page.
    assert_shows_the_record_of_a_refused_second("memory");
}

/// Rests `route` over a View whose newest second the store refused a complete reading of and
/// the worker read, and asserts the page shows what that second's record holds.
fn assert_shows_the_record_of_a_refused_second(route: &str) {
    let (dashboard, refused) = rested_over_a_refused_second(route);

    let shown = dashboard.shown_summary();

    let memory = refused.memory.unwrap();
    assert_eq!(dashboard.shown_second(), Some(10_001));
    assert_eq!(shown["cpu_percent"].as_f64(), refused.cpu_percent);
    assert_eq!(
        shown["power_watts"].as_f64(),
        refused.power_watts.map(f64::from)
    );
    assert_eq!(
        shown["cpu_celsius"].as_f64(),
        refused.cpu_celsius.map(f64::from)
    );
    assert_eq!(
        shown["gpu_celsius"].as_f64(),
        refused.gpu_celsius.map(f64::from)
    );
    assert_eq!(
        shown["soc_celsius"].as_f64(),
        refused.soc_celsius.map(f64::from)
    );
    assert_eq!(
        summary::memory_in_use_bytes(&shown),
        summary::memory_in_use_bytes_of(
            memory.app_bytes.unwrap(),
            memory.wired_bytes,
            memory.compressed_bytes
        )
    );
}

#[test]
fn a_rested_memory_page_shows_the_app_memory_of_a_second_without_a_complete_reading() {
    // Goal: a second without a complete reading shows the app memory its record holds, and no
    // summary of it makes up a purgeable reading the record does not hold. Method: rest the
    // Memory page over a refused second the worker read, then the keys of the shown summary,
    // its App memory against that of the worker's point of the same memory, and Memory in use.
    let (dashboard, refused) = rested_over_a_refused_second("memory");

    let shown = dashboard.shown_summary();

    let memory = refused.memory.unwrap();
    assert!(shown.get("memory_purgeable_bytes").is_none());
    assert!(shown.get("memory_anonymous_bytes").is_none());
    let app_memory = |summary: &Value| {
        crate::live_memory::quantities(summary)
            .into_iter()
            .find(|quantity| quantity.name == "App memory")
            .unwrap()
            .reading
    };
    assert_eq!(app_memory(&shown), app_memory(&trend_point(&refused)));
    assert_ne!(app_memory(&shown), "Unavailable");
    assert_eq!(
        summary::memory_in_use_bytes(&shown),
        summary::memory_in_use_bytes_of(
            memory.app_bytes.unwrap(),
            memory.wired_bytes,
            memory.compressed_bytes
        )
    );
}

/// A page on `route` whose reader rests over a View whose newest second, 10,001, the store
/// refused a complete reading of, which the worker read and wrote its point of a refused second
/// for, and that second.
fn rested_over_a_refused_second(route: &str) -> (LiveDashboard, MachineSecond) {
    let mut refused = reading_at(10_001, 0);
    refused.complete = viewer::store::CompleteMachineState::Refused("fixture");
    let seconds = [with_memory_reading(reading_at(10_000, 0)), refused.clone()];
    let mut reading = readings(&seconds);
    reading.selection.legacy_mut().unwrap()["summary"] = missing_point(&refused);

    (rested_on(route, &seconds, reading), refused)
}

/// The point the worker writes for `second` where the store holds no complete reading of it,
/// which names the second and holds none of its readings.
fn missing_point(second: &MachineSecond) -> Value {
    let mut point = trend_point(second);
    let named = ["run", "boot", "segment", "wall_ns", "publication"];
    for (key, value) in point.as_object_mut().unwrap() {
        if !named.contains(&key.as_str()) {
            *value = Value::Null;
        }
    }

    point
}

/// A page on `route` that took `reading`, the worker's reading of `seconds`, and rests its
/// reader over the View of the same seconds.
fn rested_on(
    route: &str,
    seconds: &[MachineSecond],
    mut reading: Box<crate::live_source::Snapshot>,
) -> LiveDashboard {
    let mut dashboard = opening_on(route);
    reading.generation = dashboard.generation;
    assert!(dashboard.receive(Update::Ready(reading)));
    dashboard.take_view(Arc::new(view(1, &[], seconds)));
    dashboard.tick_rest();
    assert!(dashboard.reader_rested());

    dashboard
}

#[test]
fn a_rested_memory_page_ranks_the_apps_of_the_views_newest_second() {
    // Goal: once Memory's reader rests, its top Apps show the newest second the store holds, as
    // its charts do. Method: the worker's reading of memory seconds 9,999 and 10,000, which ranks
    // no Apps, the View of them that rests the reader, then a View whose newest second, at
    // 10,005, holds one App that held 3 GiB resident.
    let first = memory_seconds(&[9_999, 10_000]);
    let mut dashboard = rested_on("memory", &first, worker_reading(&first));
    let seconds = memory_seconds(&[9_999, 10_000, 10_005]);
    let mut newer = view(2, &[], &seconds);
    newer.apps.replace(
        2,
        Arc::new(apps_at(
            &seconds[2],
            &[("Brave Browser", 3 * 1_073_741_824)],
        )),
    );

    assert!(dashboard.take_view(Arc::new(newer)));

    let (rankings, target) = dashboard.ranking_reading().unwrap();
    let resident = rankings
        .metrics
        .iter()
        .find(|ranking| ranking.metric == crate::live_rankings::Metric::Resident)
        .unwrap();
    assert!(dashboard.reader_rested());
    assert_eq!(target.second, 10_005);
    assert_eq!(resident.entries[0].app.name, "Brave Browser");
    assert_eq!(resident.entries[0].amount, 3 * 1_073_741_824);
}

#[test]
fn a_followed_memory_page_rests_its_reader_and_draws_each_newer_view() {
    // Goal: a Memory page following the newest second rests its reader after its first reading
    // and draws each second after it from the store's View, as Overview does. Method: the
    // worker's first reading of Memory ends at 10,000 and the page takes a View of the same
    // seconds, then three Views each one second newer arrive with no reading, and after each the
    // page's machine charts are that View's and its readings show the View's newest second.
    let first = memory_seconds(&[9_999, 10_000]);
    let mut dashboard = opening_on("memory");
    let mut reading = worker_reading(&first);
    reading.generation = dashboard.generation;
    assert!(dashboard.receive(Update::Ready(reading)));
    let generation = dashboard.generation;

    dashboard.take_view(Arc::new(view(1, &[], &first)));
    dashboard.tick_rest();

    assert!(dashboard.reader_rested());
    for (version, newest) in [(2, 10_001), (3, 10_002), (4, 10_003)] {
        let seconds = memory_seconds(&(9_999..=newest).collect::<Vec<_>>());
        let view = Arc::new(view(version, &[], &seconds));
        assert!(dashboard.take_view(Arc::clone(&view)));
        let panels = crate::live_projection::held_projection::view_panels(
            "memory",
            &view,
            levels().as_ref(),
        )
        .unwrap();
        for panel in &panels {
            assert_eq!(
                drawn(&dashboard, &panel.id).lines,
                panel.lines,
                "{}",
                panel.id
            );
        }
        assert_eq!(dashboard.shown_second(), Some(newest));
    }
    assert_eq!(dashboard.generation, generation);
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
fn a_run_that_ended_before_the_view_s_newest_second_leaves_the_status_live() {
    // Goal: the worker's answer from before a new run started says nothing of that run, since
    // the worker rests while the page draws from the View. Method: readings that end at 10,000
    // with their run completed, a View that ends at 10,005, and the clock at 10,006.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    dashboard.snapshot.as_mut().unwrap().catalog["collector_completed"] = json!(true);

    dashboard.take_view(newer_view());

    assert_eq!(
        dashboard.status_at(UNIX_EPOCH + Duration::from_secs(10_006)),
        "Live"
    );
}

#[test]
fn a_run_that_ended_at_the_view_s_newest_second_stops_the_status() {
    // Goal: a collector that really stopped still reads as stopped under a View of its seconds.
    // Method: readings that end at 10,005 with their run completed, and a View of those seconds.
    let mut dashboard = dashboard(&newer_seconds());
    let catalog = &mut dashboard.snapshot.as_mut().unwrap().catalog;
    catalog["collector_completed"] = json!(true);
    catalog["last_second"] = json!(10_005);

    dashboard.take_view(newer_view());

    assert_eq!(
        dashboard.status_at(UNIX_EPOCH + Duration::from_secs(10_006)),
        "Collector stopped"
    );
}

#[test]
fn a_run_that_ended_with_no_view_stops_the_status() {
    // Goal: a page drawn from the worker's answer alone reads a completed run as stopped.
    // Method: readings with their run completed and no View taken.
    let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
    dashboard.snapshot.as_mut().unwrap().catalog["collector_completed"] = json!(true);

    assert_eq!(
        dashboard.status_at(UNIX_EPOCH + Duration::from_secs(10_001)),
        "Collector stopped"
    );
}

#[test]
fn a_run_that_ended_with_no_seconds_anywhere_stops_the_status() {
    // Goal: a run that completed before saving any second reads as stopped, not as no readings.
    // Method: a completed run whose answer names no last second, and no View taken.
    let mut dashboard = dashboard(&[reading_at(10_000, 0)]);
    let catalog = &mut dashboard.snapshot.as_mut().unwrap().catalog;
    catalog["collector_completed"] = json!(true);
    catalog["last_second"] = json!(null);

    assert_eq!(
        dashboard.status_at(UNIX_EPOCH + Duration::from_secs(10_001)),
        "Collector stopped"
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

#[test]
fn a_rested_memory_page_reads_its_chart_of_resident_memory_by_app_at_the_views_newest_second() {
    // Goal: once Memory's reader rests, its chart of resident memory by App reads the newest
    // second the store holds as the worker's chart of the same seconds reads it, rather than the
    // history of the worker's first reading. Method: the worker's reading of 9,999 and 10,000,
    // the View of them that rests the reader, then a View that ends five seconds later and holds
    // an App in each second, beside a page showing the worker's reading and charts by App of
    // those seconds.
    let first = [reading_at(9_999, 0), reading_at(10_000, 0)];
    let mut dashboard = rested_on("memory", &first, worker_reading(&first));

    assert!(dashboard.take_view(Arc::new(charted_view())));

    let worker = drawn_by_app(&newer_seconds());
    let id = crate::live_resident_history::PANEL_ID;
    assert!(dashboard.reader_rested());
    assert_eq!(chart_reading(&dashboard, id), chart_reading(&worker, id));
}

/// The stretch of the loaded hour that also holds the Apps of `crowded_used`, a stretch the
/// Overview's default window of fifteen minutes holds wholly.
const CROWDED_STRETCH_INDEX: usize = 50;

/// Weak references to the names of Z the stretch at `CROWDED_STRETCH_INDEX` of the loaded hour
/// was made from, once `dashboard`, an Overview, takes that View and chooses its charts by App
/// over it, and the test lets go of it. That stretch also holds eight Apps more. The least of
/// them is Z, last in key order, which wins no tie on the charts every App reads none of, so no
/// chart draws it on a line of its own and keeps its name, and only the View and the room the
/// page chooses in keep it alive.
fn crowded_names(dashboard: &mut LiveDashboard) -> Vec<std::sync::Weak<RankedApp>> {
    let (mut view, _) = crate::live_store_app_fixtures::hour();
    let number = view.stretches[CROWDED_STRETCH_INDEX].stretch;
    let seconds: Vec<Arc<AppsSecond>> = (number * 10..number * 10 + 10)
        .map(|second| Arc::new(apps_at(&reading_at(second, 0), &crowded_used())))
        .collect();
    let names: Vec<std::sync::Weak<RankedApp>> = seconds
        .iter()
        .flat_map(|second| {
            second
                .apps
                .iter()
                .filter(|app| app.name.display_name == "Z")
        })
        .map(|app| Arc::downgrade(&app.name))
        .collect();
    assert_eq!(names.len(), seconds.len());
    view.stretches[CROWDED_STRETCH_INDEX] =
        Arc::new(viewer::store::AppsStretch::of_seconds(number, &seconds));
    drop(seconds);
    assert!(dashboard.take_view(Arc::new(view)));
    assert!(names.iter().any(|name| name.upgrade().is_some()));
    names
}

#[test]
fn a_page_that_cannot_draw_its_view_keeps_no_app_of_it_alive() {
    // Goal: a page that lets go of the store's View it could not draw keeps alive none of the
    // Apps it chose its charts by App from, since the room it chooses them in keeps the Apps of
    // the View's stretches between choices. Method: Overview over the View of the loaded hour
    // that `crowded_names` makes, then the page lets go of it as a failed draw does, which no
    // View a test can build reaches through a draw.
    let hour_newest = crate::live_store_app_fixtures::HOUR_NEWEST;
    let mut dashboard = dashboard(&[reading_at(hour_newest - 1, 0), reading_at(hour_newest, 0)]);
    let names = crowded_names(&mut dashboard);

    dashboard.draw_store_view_failed("The View could not be drawn".to_owned());

    assert!(names.iter().all(|name| name.upgrade().is_none()));
}

/// What the Apps of the crowded stretch use at each of its seconds, in key order: A, B and C as
/// at the hour's other seconds, seven Apps more than any of them, and Z less than any.
fn crowded_used() -> [(&'static str, u64); 11] {
    [
        ("A", 500_000_000),
        ("B", 250_000_000),
        ("C", 125_000_000),
        ("D", 900_000_000),
        ("E", 900_000_000),
        ("F", 900_000_000),
        ("G", 900_000_000),
        ("H", 900_000_000),
        ("I", 900_000_000),
        ("J", 900_000_000),
        ("Z", 1_000),
    ]
}

mod when_the_view_cannot_choose_the_apps_its_charts_draw {
    use super::*;

    #[test]
    fn the_page_draws_no_chart_by_app_and_says_why() {
        // Goal: where the page could not choose the Apps its charts draw, Overview draws none
        // of its charts by App, not even the worker's older ones, and their cards say why.
        // Method: readings with charts by App, then a newer View of twelve seconds, each of
        // whose 2,048 Apps no other second holds, 24,576 in all, more than the page adds up.
        let mut dashboard = drawn_by_app(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        let seconds: Vec<_> = (10_001..=10_012)
            .map(|second| reading_at(second, 0))
            .collect();
        let mut view = view(1, &[], &seconds);
        view.apps_loaded = true;
        view.apps = seconds
            .iter()
            .map(|second| {
                let wall_second = second.wall_ns / 1_000_000_000;
                let names: Vec<String> = (0..2_048)
                    .map(|app| format!("{wall_second}-{app:04}"))
                    .collect();
                let used: Vec<(&str, u64)> = names.iter().map(|name| (name.as_str(), 1)).collect();
                Arc::new(apps_at(second, &used))
            })
            .collect();

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

mod when_the_store_still_loads_the_apps {
    use super::*;

    #[test]
    fn the_page_draws_no_chart_by_app() {
        // Goal: while the store still loads the Apps of the seconds it holds, Overview draws
        // none of its charts by App, not even the worker's older ones, rather than lines of the
        // Apps that led over the seconds loaded so far. Method: readings with charts by App,
        // then the newer View of the chart test, whose every second holds Brave Browser, marked
        // as still loading its Apps.
        let mut dashboard = drawn_by_app(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        let mut view = charted_view();
        view.apps_loaded = false;

        dashboard.take_view(Arc::new(view));

        let cpu = dashboard.attribution_history(Metric::Cpu).unwrap();
        assert_eq!(by_app(&dashboard.panels), Vec::new());
        assert_eq!(
            cpu.coverage(None),
            "App rate history unavailable: App rate history is empty"
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

        let expected = summary::cards(
            "overview",
            &crate::live_store_fixtures::selection_of_json(&summary_selection(&reading_at(
                10_005, 0,
            ))),
        );
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
        // Goal: a page with no chart by App the View draws keeps the charts of its own
        // readings. Method: the Network page, then a newer View.
        let mut dashboard = dashboard_on("network", &[reading_at(9_999, 0), reading_at(10_000, 0)]);
        let before = dashboard.panels.clone();

        let changed = dashboard.take_view(newer_view());

        assert!(!changed);
        assert_same_panels(&dashboard.panels, &before);
    }
}

mod when_the_page_has_a_chart_by_app {
    use super::*;
    use crate::live_source::AppChartSource;

    #[test]
    fn the_cpu_page_draws_its_chart_by_app_from_the_view() {
        // Goal: the CPU page draws its chart by App from the store's View, as Overview does.
        // Method: the page showing the worker's readings, which draw no chart by App, then a
        // View that holds an App in each second.
        let view = Arc::new(charted_view());

        let drawn = drawn_on_page("cpu", &view);

        assert_eq!(drawn, expected_on_page(&view, Metric::Cpu.panel_id()));
    }

    #[test]
    fn the_energy_page_draws_its_chart_by_app_from_the_view() {
        // Goal: the Energy page draws its chart by App from the store's View. Method: as for
        // the CPU page.
        let view = Arc::new(charted_view());

        let drawn = drawn_on_page("energy", &view);

        assert_eq!(drawn, expected_on_page(&view, Metric::Power.panel_id()));
    }

    #[test]
    fn the_storage_page_draws_its_chart_by_app_from_the_view() {
        // Goal: the Storage page draws its chart by App from the store's View. Method: as for
        // the CPU page.
        let view = Arc::new(charted_view());

        let drawn = drawn_on_page("storage", &view);

        assert_eq!(drawn, expected_on_page(&view, Metric::Disk.panel_id()));
    }

    #[test]
    fn the_gpu_page_draws_its_chart_by_app_from_the_view() {
        // Goal: the GPU page draws its chart by App from the store's View, which adds up GPU
        // time in its stretches too. Method: as for the CPU page.
        let view = Arc::new(charted_view());

        let drawn = drawn_on_page("gpu", &view);

        assert_eq!(drawn, expected_on_page(&view, Metric::Gpu.panel_id()));
    }

    #[test]
    fn the_memory_page_draws_its_chart_by_app_from_the_view() {
        // Goal: the Memory page draws its chart of resident memory by App from the store's
        // View. Method: as for the CPU page.
        let view = Arc::new(charted_view());

        let drawn = drawn_on_page("memory", &view);

        let expected = expected_on_page(&view, crate::live_resident_history::PANEL_ID);
        assert_eq!(drawn, expected);
    }

    #[test]
    fn a_page_opened_from_another_draws_its_chart_by_app_from_the_same_view() {
        // Goal: a page opened from another page draws its chart by App from the View that page
        // drew, before the store hands over a newer one. Method: the CPU page takes a View,
        // then the GPU page opens and the worker reads it.
        let view = Arc::new(charted_view());
        let trends = [reading_at(9_999, 0), reading_at(10_000, 0)];
        let mut dashboard = dashboard_on("cpu", &trends);
        assert!(dashboard.take_view(Arc::clone(&view)));

        dashboard.switch_route("gpu");
        dashboard.accept(readings(&trends));

        let expected = expected_on_page(&view, Metric::Gpu.panel_id());
        assert_eq!(by_app(&dashboard.panels), expected);
    }

    #[test]
    fn a_page_the_worker_answers_under_a_hover_reads_none_of_overviews_charts_by_app() {
        // Goal: a page opened from Overview that the worker answers under a hover, which holds
        // the page's charts still, reads its charts by App from the worker and never the ones
        // Overview drew from the View. Method: Overview takes a View, the CPU page opens, a
        // hover of the worker's readings begins, and the worker answers with readings that draw
        // no chart by App.
        let trends = [reading_at(9_999, 0), reading_at(10_000, 0)];
        let mut dashboard = dashboard_on("overview", &trends);
        assert!(dashboard.take_view(Arc::new(charted_view())));
        dashboard.switch_route("cpu");
        dashboard.preview = Some(preview(&dashboard));

        dashboard.accept(readings(&trends));

        assert!(dashboard.attribution_history(Metric::Cpu).is_none());
    }

    mod what_draws_it {
        use super::*;

        #[test]
        fn the_view_draws_the_cpu_page_s_chart_once_the_page_has_a_reading() {
            // Goal: a page whose chart by App the View draws says so, so the worker builds no
            // App history of it. Method: the CPU page with the worker's readings takes a View.
            let dashboard = drawn_from_view("cpu");

            assert_eq!(dashboard.app_chart_source(), AppChartSource::View);
        }

        #[test]
        fn the_view_draws_the_memory_page_s_chart_once_the_page_has_a_reading() {
            // Goal: as on the CPU page, the Memory page says the View draws its chart of
            // resident memory. Method: as on the CPU page.
            let dashboard = drawn_from_view("memory");

            assert_eq!(dashboard.app_chart_source(), AppChartSource::View);
        }

        #[test]
        fn the_reading_draws_it_before_the_page_s_first_reading() {
            // Goal: a newly opened page draws its chart by App from the worker's first reading,
            // so that reading brings the history and the chart shows at once. Method: the CPU
            // page, newly opened, takes a View before the worker answers.
            let mut dashboard = opening_on("cpu");

            dashboard.take_view(Arc::new(charted_view()));

            assert_eq!(dashboard.app_chart_source(), AppChartSource::Reading);
        }

        #[test]
        fn the_reading_draws_it_at_a_second_chosen_without_a_hold() {
            // Goal: a chosen second draws the charts of its own readings, so its reading brings
            // the history. Method: the CPU page drawn from a View chooses a second.
            let mut dashboard = drawn_from_view("cpu");

            dashboard.request.second = Some(10_000);

            assert_eq!(dashboard.app_chart_source(), AppChartSource::Reading);
        }

        #[test]
        fn the_reading_draws_it_in_a_saved_recording() {
            // Goal: a window that opened a saved recording draws only what its worker reads.
            // Method: the CPU page drawn from a View reads a saved recording instead.
            let mut dashboard = drawn_from_view("cpu");

            dashboard.native.source = crate::live_source_mode::SourceMode::Recorded;

            assert_eq!(dashboard.app_chart_source(), AppChartSource::Reading);
        }

        #[test]
        fn the_view_draws_it_while_a_second_is_held() {
            // Goal: a held second keeps the chart drawn from the View it was held over, so the
            // reading of the held page needs no history. Method: the CPU page drawn from a View
            // holds a second of it.
            let mut dashboard = drawn_from_view("cpu");

            hold_second(&mut dashboard);

            assert_eq!(dashboard.app_chart_source(), AppChartSource::View);
        }

        #[test]
        fn a_held_second_reads_the_chart_drawn_from_the_view() {
            // Goal: a held second shows the chart and readouts the View drew, whatever the
            // reading brought. Method: the CPU page drawn from a View holds a second of it.
            let mut dashboard = drawn_from_view("cpu");
            let drawn = std::ptr::from_ref(dashboard.attribution_history(Metric::Cpu).unwrap());

            hold_second(&mut dashboard);

            let held = dashboard.attribution_history(Metric::Cpu).unwrap();
            assert!(std::ptr::eq(held, drawn));
        }

        #[test]
        fn the_reading_draws_overview_s_charts_by_app() {
            // Goal: Overview's readings go on bringing its charts by App as before. Method:
            // Overview with the worker's readings takes a View.
            let dashboard = drawn_from_view("overview");

            assert_eq!(dashboard.app_chart_source(), AppChartSource::Reading);
        }

        #[test]
        fn the_reading_draws_it_once_the_window_hides() {
            // Goal: a hidden window lets go of the View, so its page shown again reads the
            // history unless a View draws it first. Method: the CPU page drawn from a View hides.
            let mut dashboard = drawn_from_view("cpu");

            dashboard.tick_hidden();

            assert_eq!(dashboard.app_chart_source(), AppChartSource::Reading);
        }

        #[test]
        fn a_hidden_window_keeps_the_chart_it_drew() {
            // Goal: the chart drawn from the View stays drawn while the window hides, so the
            // window shown again shows no blank chart before its next reading. Method: the CPU
            // page drawn from a View hides.
            let mut dashboard = drawn_from_view("cpu");
            let view = charted_view();

            dashboard.tick_hidden();

            let expected = expected_on_page(&view, Metric::Cpu.panel_id());
            assert_eq!(by_app(&dashboard.panels), expected);
        }

        #[test]
        fn choosing_a_second_tells_the_worker_before_it_reads_the_second() {
            // Goal: a chosen second is read once, so the worker learns the reading draws the
            // chart before the request for that read goes out. Method: the CPU page drawn from
            // a View, which the worker was told, chooses a second and asks for it, and the
            // worker's mailbox keeps what the worker had been told as the request arrived.
            let mut dashboard = drawn_from_view("cpu");
            dashboard.worker.set_app_chart_source(AppChartSource::View);
            dashboard.request.second = Some(10_000);

            dashboard.request_reading();

            let sent = dashboard.worker.sent_app_chart_source();
            assert_eq!(sent, Some(AppChartSource::Reading));
        }

        #[test]
        fn the_reading_draws_it_while_a_hover_holds_the_page_s_first_view_back() {
            // Goal: a hover shown as the page's first View arrives holds the charts the reading
            // drew, so the reading goes on bringing the history they draw. Method: the CPU page
            // with the worker's readings shows a hover of them, then takes its first View.
            let mut dashboard = dashboard_on("cpu", &trends());
            dashboard.preview = Some(preview(&dashboard));

            dashboard.take_view(Arc::new(charted_view()));

            assert_eq!(dashboard.app_chart_source(), AppChartSource::Reading);
        }

        #[test]
        fn a_hover_reads_the_chart_drawn_from_the_view() {
            // Goal: a hover on a page whose reading left its chart by App to the View reads the
            // chart the View drew, and the worker goes on building none. Method: the CPU page
            // drawn from a View takes a reading that left the chart to it, then a hover of the
            // worker's readings begins.
            let mut dashboard = drawn_from_view("cpu");
            dashboard.accept(left_to_the_view(&trends()));
            let drawn = std::ptr::from_ref(dashboard.attribution_history(Metric::Cpu).unwrap());

            dashboard.preview = Some(preview(&dashboard));

            let hovered = dashboard.attribution_history(Metric::Cpu).unwrap();
            assert!(std::ptr::eq(hovered, drawn));
            assert_eq!(dashboard.app_chart_source(), AppChartSource::View);
        }

        #[gpui::test]
        fn a_window_opened_again_draws_the_chart_its_reading_left_to_the_view(
            cx: &mut gpui::TestAppContext,
        ) {
            // Goal: a window closed and opened again shows the chart its reading left to the
            // View, with its history, before the store's next View or the reader's next
            // reading. Method: the CPU page drawn from a View takes a reading that left the
            // chart to it, the window closes, and a window opens again from what it kept.
            use gpui::AppContext;
            let mut dashboard = drawn_from_view("cpu");
            dashboard.accept(left_to_the_view(&trends()));
            let state = dashboard.window_state();
            let worker = opening_on("cpu").worker;

            let page = cx.new(|cx| LiveDashboard::restore(worker, state, cx));

            let view = charted_view();
            cx.update(|cx| {
                let page = page.read(cx);
                let expected = expected_on_page(&view, Metric::Cpu.panel_id());
                assert_eq!(by_app(&page.panels), expected);
                assert!(page.attribution_history(Metric::Cpu).is_some());
            });
        }

        #[test]
        fn a_window_closed_keeps_the_charts_only_for_a_reading_that_left_them_to_the_view() {
            // Goal: a closed window holds the charts drawn from the View only for a reading
            // that left its chart by App to them, so it never holds a chart its reading brought
            // a second time. Method: the CPU page drawn from a View closes once after a reading
            // that left the chart to the View, and once after a reading that brought it.
            let mut left = drawn_from_view("cpu");
            left.accept(left_to_the_view(&trends()));
            let mut brought = drawn_from_view("cpu");
            brought.accept(readings(&trends()));

            let left = left.window_state();
            let brought = brought.window_state();

            assert!(left.store_panels.version.is_some());
            assert!(brought.store_panels.version.is_none());
        }

        /// Holds the View's newest second on `dashboard`, as a click on a chart does.
        fn hold_second(dashboard: &mut LiveDashboard) {
            let reading = preview(dashboard);
            dashboard.request.second = Some(reading.target.second);
            dashboard.hold = Some(crate::live_dashboard::hold::Hold {
                last: 10_005,
                anchor_second: reading.target.second,
                reading,
            });
        }
    }

    mod when_the_view_falls_behind_a_reading_that_left_it_the_chart {
        use super::*;

        #[test]
        fn the_view_goes_on_drawing_the_chart() {
            // Goal: a reading that left the chart by App to the View brings none of it, so a
            // View a second behind that reading still draws it and the chart is never blank.
            // Method: the CPU page drawn from a View that ends at 10,005, then a reading that
            // left the chart to it and ends at 10,006.
            let view = charted_view();
            let mut dashboard = drawn_from_view("cpu");

            dashboard.accept(left_to_the_view(&trends_past_view()));

            let expected = expected_on_page(&view, Metric::Cpu.panel_id());
            assert_eq!(by_app(&dashboard.panels), expected);
        }

        #[test]
        fn the_page_s_next_reading_brings_the_chart() {
            // Goal: a View that fell behind the page's reading leaves the next reading to bring
            // the chart, so the chart lags the page by one reading at most. Method: the CPU page
            // drawn from a View that ends at 10,005, then a reading that left the chart to it
            // and ends at 10,006.
            let mut dashboard = drawn_from_view("cpu");

            dashboard.accept(left_to_the_view(&trends_past_view()));

            assert_eq!(dashboard.app_chart_source(), AppChartSource::Reading);
        }
    }

    mod when_the_view_falls_behind_a_reading_that_carries_the_chart {
        use super::*;

        #[test]
        fn the_reading_draws_the_chart() {
            // Goal: a store that falls behind the worker never draws less than the worker read.
            // Method: the CPU page drawn from a View that ends at 10,005, then a reading that
            // ends at 10,006 and carries the CPU history by App.
            let view = charted_view();
            let mut dashboard = drawn_from_view("cpu");
            let mut snapshot = readings(&trends_past_view());
            let history =
                AppAttributionHistory::from_view(Metric::Cpu, &view, &charted_cohorts(&view));
            snapshot.app_attribution_history =
                Some(Arc::new(crate::live_source::Presentation::Legacy(history)));

            dashboard.accept(snapshot);

            let read = dashboard.snapshot.as_ref().unwrap();
            let read: &AppAttributionHistory = read.app_attribution_history.as_deref().unwrap();
            let shown = dashboard.attribution_history(Metric::Cpu).unwrap();
            assert!(std::ptr::eq(shown, read));
        }
    }

    mod when_a_page_kept_is_opened_again {
        use super::*;

        #[test]
        fn the_view_draws_its_chart_by_app() {
            // Goal: a page opened again from the pages kept shows its chart by App from the View
            // at once, though its kept reading brought none. Method: the CPU page drawn from a
            // View, the Memory page opened and read, then the CPU page opened again.
            let view = Arc::new(charted_view());
            let trends = [reading_at(9_999, 0), reading_at(10_000, 0)];
            let mut dashboard = dashboard_on("cpu", &trends);
            assert!(dashboard.take_view(Arc::clone(&view)));
            dashboard.switch_route("memory");
            dashboard.accept(readings(&trends));

            dashboard.switch_route("cpu");

            let expected = expected_on_page(&view, Metric::Cpu.panel_id());
            assert_eq!(by_app(&dashboard.panels), expected);
        }
    }

    mod when_the_view_leaves_out_the_memory_chart {
        use super::*;

        #[test]
        fn the_card_says_why() {
            // Goal: the Memory page whose reading left its chart of resident memory to the View
            // says why the View left the chart out, as it said when the reading drew it.
            // Method: the Memory page takes a View whose Apps the store still loads, then a
            // reading that left the chart to it.
            let mut dashboard = dashboard_on("memory", &trends());
            let mut view = charted_view();
            view.apps_loaded = false;
            assert!(dashboard.take_view(Arc::new(view)));

            dashboard.accept(left_to_the_view(&trends()));

            let reading = dashboard.memory_attribution_absent();
            let expected = "Resident App history unavailable: Resident App history is empty";
            assert_eq!(reading, expected);
        }
    }

    /// The worker's readings of the two seconds before a `charted_view`.
    fn trends() -> [MachineSecond; 2] {
        [reading_at(9_999, 0), reading_at(10_000, 0)]
    }

    /// The worker's readings of the newest second of a `charted_view` and of the one after it.
    fn trends_past_view() -> [MachineSecond; 2] {
        [reading_at(10_005, 0), reading_at(10_006, 0)]
    }

    /// The page `route` with the worker's readings, drawn from a `charted_view`.
    fn drawn_from_view(route: &str) -> LiveDashboard {
        let mut dashboard = dashboard_on(route, &trends());
        assert!(dashboard.take_view(Arc::new(charted_view())));
        dashboard
    }

    /// The worker's readings of `trends`, read while the page said the View draws its chart by
    /// App, so they bring no App history.
    fn left_to_the_view(trends: &[MachineSecond]) -> Box<crate::live_source::Snapshot> {
        let mut snapshot = readings(trends);
        snapshot.app_chart_source = AppChartSource::View;
        snapshot
    }

    /// The charts by App the page `route` draws once it takes `view`, after the worker's
    /// readings of the seconds before it.
    fn drawn_on_page(route: &str, view: &Arc<View>) -> Vec<(String, String, Vec<Line>)> {
        let mut dashboard = dashboard_on(route, &[reading_at(9_999, 0), reading_at(10_000, 0)]);
        assert!(by_app(&dashboard.panels).is_empty());

        assert!(dashboard.take_view(Arc::clone(view)));
        by_app(&dashboard.panels)
    }

    /// The one chart by App, of id `id`, that a page draws from `view`, a `charted_view`.
    fn expected_on_page(view: &View, id: &str) -> Vec<(String, String, Vec<Line>)> {
        let cohorts = charted_cohorts(view);
        let panel = match Metric::from_panel(id) {
            Some(metric) => AppAttributionHistory::from_view(metric, view, &cohorts).panel(),
            None => ResidentHistory::from_view(view, &cohorts).panel(),
        };
        let panel = panel.unwrap();

        vec![(panel.id.clone(), panel.title.clone(), panel.lines.clone())]
    }
}

mod when_the_view_is_placed_before_the_reading_of_its_tick {
    use super::*;
    use crate::chart_axes::TimeWindow;
    use crate::live_source::{AppChartSource, Snapshot};
    use viewer::store::WallSeconds;

    #[test]
    fn the_memory_page_ends_as_after_the_reading_and_then_the_view() {
        // Goal: a page other than Overview that places the store's View before it takes the
        // worker's reading of the same tick ends as it would taking the reading with the
        // earlier View and then the new View, today's order. Method: two Memory pages drawn
        // from one View take a reading that left their chart by App to the View and a newer
        // View, each in one order, and their states are compared.
        let (taken_after, placed_before) = both_orders("memory", left_to_the_view);

        assert_eq!(placed_before, taken_after);
    }

    #[test]
    fn the_cpu_page_ends_as_after_the_reading_and_then_the_view() {
        // Goal: as on the Memory page, for a chart by App of a metric the reading would bring.
        // Method: as on the Memory page, on the CPU page.
        let (taken_after, placed_before) = both_orders("cpu", left_to_the_view);

        assert_eq!(placed_before, taken_after);
    }

    #[test]
    fn overview_ends_as_after_the_reading_and_then_the_view() {
        // Goal: Overview, which draws its machine charts and charts by App from the View
        // through a branch of its own, ends as in today's order too. Method: as on the Memory
        // page, on Overview, whose readings bring its charts by App.
        let (taken_after, placed_before) = both_orders("overview", carrying_the_chart);

        assert_eq!(placed_before, taken_after);
    }

    #[test]
    fn a_page_that_draws_nothing_from_the_view_ends_as_after_the_reading_and_then_the_view() {
        // Goal: where the page draws nothing from the newer View, since the View trails a
        // reading that brought the chart by App, the reading's own charts stand as in today's
        // order. Method: as on the Memory page, with a reading past the newer View that
        // carries the chart.
        let (taken_after, placed_before) = both_orders("memory", past_the_view);

        assert_eq!(placed_before, taken_after);
    }

    #[test]
    fn the_page_builds_its_charts_by_app_once() {
        // Goal: placing the View first builds the charts by App once, for the reading and the
        // View together. Method: the Memory page of the first test that places the View
        // first, counting its builds after it was drawn from one View.
        let mut placed_before = drawn_from_the_earlier_view("memory");
        let built = placed_before.store_panels.builds;

        place_the_view_and_then_take_the_reading(&mut placed_before, left_to_the_view());

        assert_eq!(placed_before.store_panels.builds, built + 1);
    }

    mod when_the_page_refuses_the_reading {
        use super::*;

        #[test]
        fn the_memory_page_draws_nothing_from_the_view_in_either_order() {
            // Goal: where a page other than Overview refuses the reading, the View draws
            // nothing whether the page takes it after the reading or places it before, so a
            // tick that placed it first owes the page no draw. Method: two Memory pages drawn
            // from one View take a reading whose context ends before its second and a newer
            // View, each in one order.
            let (drawn_after, taken_after, placed_before) = refused_in_both_orders("memory");

            assert!(!drawn_after);
            assert_eq!(placed_before, taken_after);
        }

        #[test]
        fn overview_draws_nothing_from_the_view_in_either_order() {
            // Goal: as on the Memory page, on Overview, which draws its machine charts from
            // the View through a branch of its own. Method: as on the Memory page.
            let (drawn_after, taken_after, placed_before) = refused_in_both_orders("overview");

            assert!(!drawn_after);
            assert_eq!(placed_before, taken_after);
        }

        /// Whether the newer View drew on a page `route` that refused a reading and then took
        /// it, that page's state, and the state of a page that placed the View and then
        /// refused the reading.
        fn refused_in_both_orders(route: &str) -> (bool, State, State) {
            let mut taken_after = drawn_from_the_earlier_view(route);
            let mut placed_before = drawn_from_the_earlier_view(route);

            taken_after.accept(refused());
            let drawn_after = taken_after.take_view(newer_charted_view());
            placed_before.store_view_place(newer_charted_view());
            placed_before.accept(refused());

            assert!(taken_after.error.is_some());
            assert_eq!(placed_before.error, taken_after.error);
            (drawn_after, state(&taken_after), state(&placed_before))
        }
    }

    mod when_the_page_cannot_draw_the_view {
        use super::*;

        #[test]
        fn overview_shows_the_readings_charts_whole() {
            // Goal: where Overview cannot draw the View placed first, it shows the reading's
            // own charts whole, as a page that holds no View does, and reports no failure of
            // its reading. Method: Overview drawn from a View places a newer View whose newest
            // power reading is no number and takes a reading, and an Overview that never held
            // a View takes the same reading.
            let mut placed_before = drawn_from_the_earlier_view("overview");
            let trends = [reading_at(9_999, 0), reading_at(10_000, 0)];
            let mut without_a_view = dashboard_on("overview", &trends);

            placed_before.store_view_place(view_that_cannot_be_drawn());
            placed_before.accept(carrying_the_chart());
            without_a_view.accept(carrying_the_chart());

            assert!(placed_before.store_view.is_none());
            assert!(placed_before.error.is_none());
            assert_eq!(charts(&placed_before), charts(&without_a_view));
        }

        /// A View past `charted_view` whose newest power reading is no number.
        fn view_that_cannot_be_drawn() -> Arc<View> {
            let mut seconds = [
                reading_at(9_999, 0),
                reading_at(10_000, 0),
                reading_at(10_005, 0),
            ];
            seconds[2].power_watts = Some(f32::NAN);
            Arc::new(view(2, &[], &seconds))
        }
    }

    /// What the page shows and what it drew it from.
    #[derive(Debug, PartialEq)]
    struct State {
        panels: Vec<Panel>,
        rows: usize,
        window: Option<TimeWindow>,
        chosen_over: Option<WallSeconds>,
        drawn_version: Option<u64>,
        app_chart_source: AppChartSource,
        last_second: Option<u64>,
        view_version: Option<u64>,
    }

    /// The states of two pages `route` drawn from an earlier View, of which one takes the
    /// reading `reading` gives and then a newer View, and the other places the newer View and
    /// then takes the reading.
    fn both_orders(route: &str, reading: fn() -> Box<Snapshot>) -> (State, State) {
        let mut taken_after = drawn_from_the_earlier_view(route);
        let mut placed_before = drawn_from_the_earlier_view(route);

        take_the_reading_and_then_the_view(&mut taken_after, reading());
        place_the_view_and_then_take_the_reading(&mut placed_before, reading());

        (state(&taken_after), state(&placed_before))
    }

    fn take_the_reading_and_then_the_view(page: &mut LiveDashboard, reading: Box<Snapshot>) {
        page.accept(reading);
        page.take_view(newer_charted_view());
        assert!(page.error.is_none());
    }

    fn place_the_view_and_then_take_the_reading(page: &mut LiveDashboard, reading: Box<Snapshot>) {
        page.store_view_place(newer_charted_view());
        page.accept(reading);
        assert!(page.error.is_none());
    }

    fn state(page: &LiveDashboard) -> State {
        State {
            panels: charts(page),
            rows: page.rows.len(),
            window: page.view_window(),
            chosen_over: page.store_panels.chosen_over,
            drawn_version: page.store_panels.version,
            app_chart_source: page.app_chart_source(),
            last_second: page.store_view_last_second(),
            view_version: page.store_view.as_ref().map(|view| view.version),
        }
    }

    /// The charts the page shows, each as it is drawn.
    fn charts(page: &LiveDashboard) -> Vec<Panel> {
        page.panels
            .iter()
            .map(|panel| Panel::clone(panel))
            .collect()
    }

    /// The page `route` showing the worker's readings of the two seconds before a
    /// `charted_view`, drawn from that View.
    fn drawn_from_the_earlier_view(route: &str) -> LiveDashboard {
        let mut page = dashboard_on(route, &[reading_at(9_999, 0), reading_at(10_000, 0)]);
        assert!(page.take_view(Arc::new(charted_view())));
        page
    }

    /// The worker's reading of the newest second of a `charted_view`, read while the page said
    /// the View draws its chart by App.
    fn left_to_the_view() -> Box<Snapshot> {
        let mut snapshot = carrying_the_chart();
        snapshot.app_chart_source = AppChartSource::View;
        snapshot
    }

    /// The worker's reading of the newest second of a `charted_view`.
    fn carrying_the_chart() -> Box<Snapshot> {
        readings(&[reading_at(10_000, 0), reading_at(10_005, 0)])
    }

    /// A reading the page refuses, whose context ends before its second.
    fn refused() -> Box<Snapshot> {
        let mut reading = carrying_the_chart();
        reading.selection.legacy_mut().unwrap()["second"] = serde_json::json!(10_005);
        reading.selection.legacy_mut().unwrap()["context_last_second"] = serde_json::json!(10_000);
        reading
    }

    /// The worker's reading of a second past `newer_charted_view`, which brought its chart by
    /// App.
    fn past_the_view() -> Box<Snapshot> {
        readings(&[reading_at(10_006, 0), reading_at(10_007, 0)])
    }

    /// A `charted_view` the store published one second later, as version 2.
    fn newer_charted_view() -> Arc<View> {
        let seconds = [
            reading_at(9_999, 0),
            reading_at(10_000, 0),
            reading_at(10_005, 0),
            reading_at(10_006, 0),
        ];
        let mut view = view(2, &[], &seconds);
        view.apps_loaded = true;
        view.apps = seconds
            .iter()
            .map(|second| Arc::new(apps_of(second, "Brave Browser", 750_000_000)))
            .collect();
        Arc::new(view)
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
        hover.reading.selection = crate::live_source::Presentation::Legacy(
            viewer::Selection::from_json(summary_selection(&reading_at(9_999, 0))),
        );
        dashboard.preview = Some(hover);

        dashboard.take_view(newer_view());

        let expected = summary::cards(
            "overview",
            &crate::live_store_fixtures::selection_of_json(&summary_selection(&reading_at(
                9_999, 0,
            ))),
        );
        assert_eq!(texts(&dashboard.summary_cards()), texts(&expected));
    }

    #[test]
    fn the_hardware_figure_shows_the_hovered_second() {
        // Goal: the figure reads the second under the pointer, as the readouts do, rather than
        // the View's newest. Method: a hover's reading of 9,999, whose CPU was 99.5% busy, then
        // a View that ends at 10,005, whose CPU was 2.5% busy.
        let mut dashboard = dashboard(&[reading_at(9_999, 0), reading_at(10_000, 0)]);
        let mut hover = preview(&dashboard);
        hover.reading.selection = crate::live_source::Presentation::Legacy(
            viewer::Selection::from_json(summary_selection(&reading_at(9_999, 0))),
        );
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

    #[test]
    fn the_page_keeps_no_app_of_the_view_alive() {
        // Goal: a page that lets go of the store's View for good, as a hidden window does,
        // keeps alive none of the Apps it chose its charts by App from, since the room it
        // chooses them in keeps the Apps of the View's stretches between choices. Method:
        // Overview over the View of the loaded hour that `crowded_names` makes, then the
        // window hides.
        let hour_newest = crate::live_store_app_fixtures::HOUR_NEWEST;
        let mut dashboard =
            dashboard(&[reading_at(hour_newest - 1, 0), reading_at(hour_newest, 0)]);
        let names = crowded_names(&mut dashboard);

        dashboard.tick_hidden();

        assert!(names.iter().all(|name| name.upgrade().is_none()));
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
    let mut dashboard = opening();
    dashboard.accept(worker_reading(seconds));
    assert!(dashboard.error.is_none());
    dashboard
}

/// The worker's readings of `seconds`, with the summary of the newest, as the worker reads them.
fn worker_reading(seconds: &[MachineSecond]) -> Box<crate::live_source::Snapshot> {
    let mut snapshot = readings(seconds);
    snapshot.selection.legacy_mut().unwrap()["summary"] = trend_point(seconds.last().unwrap());
    snapshot
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
    view.apps.replace(
        2,
        Arc::new(apps_of(&seconds[2], "Brave Browser", 750_000_000)),
    );
    Arc::new(view)
}

/// The View of `newer_seconds`, in each of which one App, Brave Browser, spent 0.75 s of CPU,
/// and whose charts by App draw it on a line of its own.
fn charted_view() -> View {
    let seconds = newer_seconds();
    let mut view = view(1, &[], &seconds);
    view.apps_loaded = true;
    view.apps = seconds
        .iter()
        .map(|second| Arc::new(apps_of(second, "Brave Browser", 750_000_000)))
        .collect();
    view
}

/// The Apps the charts by App of `view`, a `charted_view`, draw on lines of their own: Brave
/// Browser on each, over every second of `view`.
fn charted_cohorts(view: &View) -> Cohorts {
    let brave = Arc::new(RankedApp::named(
        KeyKind::Bundle,
        b"Brave Browser".to_vec(),
        "Brave Browser".into(),
    ));

    Cohorts {
        seconds: view.seconds.len(),
        cpu: vec![Arc::clone(&brave)],
        power: vec![Arc::clone(&brave)],
        disk: vec![Arc::clone(&brave)],
        gpu: vec![Arc::clone(&brave)],
        resident: vec![brave],
        ..Cohorts::default()
    }
}

/// An Overview showing the worker's readings of `trends` with the charts by App the worker
/// drew of them.
fn drawn_by_app(trends: &[MachineSecond]) -> LiveDashboard {
    let charted = charted_view();
    let cohorts = charted_cohorts(&charted);
    let history = |metric| {
        Arc::new(crate::live_source::Presentation::Legacy(
            AppAttributionHistory::from_view(metric, &charted, &cohorts),
        ))
    };
    let mut snapshot = readings(trends);
    snapshot.overview_app_histories = Some(Arc::new(OverviewAppHistories {
        cpu: history(Metric::Cpu),
        power: history(Metric::Power),
        disk: history(Metric::Disk),
    }));
    snapshot.resident_history = Some(std::sync::Arc::new(
        crate::live_source::Presentation::Shared(Arc::new(ResidentHistory::from_view(
            &charted, &cohorts,
        ))),
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
        Metric::Gpu.panel_id(),
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
    let cohorts = charted_cohorts(view);
    let resident = ResidentHistory::from_view(view, &cohorts).panel().unwrap();

    let mut expected = vec![(
        resident.id.clone(),
        "Memory by app".to_owned(),
        resident.lines.clone(),
    )];
    expected.extend(titled.into_iter().map(|(metric, title)| {
        let panel = AppAttributionHistory::from_view(metric, view, &cohorts)
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

/// The second at `wall_second` whose system-on-chip temperature, fan speeds and battery charge
/// differ from those of every other second.
fn cooled_at(wall_second: u64) -> MachineSecond {
    let tenths = f32::from(u16::try_from(wall_second % 1_000).unwrap()) / 10.0;

    MachineSecond {
        soc_celsius: Some(30.0 + tenths),
        fan_rpm: [Some(1_000.0 + tenths * 10.0), Some(2_000.0 + tenths * 20.0)],
        battery: Some(BatterySecond {
            charge_percent: Some(f64::from(tenths)),
            power_watts: None,
        }),
        ..reading_at(wall_second, 0)
    }
}

/// The seconds at `wall_seconds`, each with a complete reading of the memory it holds and of
/// memory's rates, as the store holds a second it read.
fn memory_seconds(wall_seconds: &[u64]) -> Vec<MachineSecond> {
    wall_seconds
        .iter()
        .map(|&wall_second| with_memory_reading(reading_at(wall_second, 0)))
        .collect()
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
    // Goal: native visibility controls the Store's work.
    // Method: hide and show a real window over a registered source and observe store parking.
    use gpui::{AppContext, WindowOptions, WindowVisibility};
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let (store, handle) = crate::live_source::store_start(&recording.root).unwrap();
    let worker = Worker::start_from_store(recording.root.clone(), handle).unwrap();
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
fn showing_a_pinned_window_wakes_the_store_and_keeps_the_second(cx: &mut gpui::TestAppContext) {
    // Goal: a pinned window shown again wakes the Store, which goes on taking in readings, and
    // keeps the second it holds. Method: show a pinned window over a paused real Store.
    use gpui::{AppContext, WindowOptions, WindowVisibility};
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let (store, handle) = crate::live_source::store_start(&recording.root).unwrap();
    let worker = Worker::start_from_store(recording.root.clone(), handle).unwrap();
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

    assert!(cx.update(|cx| cx.global::<super::LiveStore>().0.is_active()));
    let held = window
        .update(cx, |dashboard, _, _| dashboard.request.second)
        .unwrap();
    assert_eq!(held, Some(1000));
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
fn a_live_page_other_than_overview_keeps_the_store_ingesting(cx: &mut gpui::TestAppContext) {
    // Goal: the store follows the live Mac while another page shows, so Overview opened again
    // has no missed minutes to catch up from.
    // Method: tick CPU over an installed live store that starts parked, and watch it resume.
    use gpui::AppContext;
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let store = recording.start_store();
    store.set_active(false);
    cx.update(|cx| super::install_store(store, cx));
    wait_for_store_activity(cx, true);
    let page = cx.update(|cx| cx.new(|_| opening_on("cpu")));

    page.update(cx, LiveDashboard::tick);

    wait_for_store_activity(cx, false);
    assert!(cx.update(|cx| cx.global::<super::LiveStore>().0.is_active()));
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

#[gpui::test]
fn a_followed_cpu_page_drawn_from_the_view_reads_no_app_history(cx: &mut gpui::TestAppContext) {
    // Goal: while the CPU page draws its chart by App from the store's View, the worker's
    // reading of each new second builds no App history of it and still ranks the Apps a hover
    // reads, and the page reads the chart the View drew. Method: the CPU page over a real store
    // and reader settles on the View, then the recording publishes a second and the page takes
    // the worker's reading of it.
    let (recording, page) = followed_page_read_again(cx, "cpu");

    cx.update(|cx| {
        let page = page.read(cx);
        let snapshot = page.snapshot.as_ref().unwrap();
        assert!(snapshot.app_attribution_history.is_none());
        assert!(snapshot.app_ranks.is_some());
        assert!(page.attribution_history(Metric::Cpu).is_some());
    });
    drop(recording);
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

#[gpui::test]
fn a_followed_memory_page_reads_no_resident_history_past_its_first_reading(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: the Memory page's reader builds no resident history while the View draws that
    // chart, since the page rests its reader once it took its first reading and draws each
    // newer second's history of resident memory from the View. Method: a Memory page over a real
    // store and reader settles on the View, the recording publishes a third second, and the page
    // ticks until it draws that second from the View, then the second the worker's reading
    // selects and the history of resident memory the page reads.
    let (mut recording, page) = followed_page_settled(cx, "memory");
    let read =
        cx.update(|cx| page.read(cx).snapshot.as_ref().unwrap().selection["second"].as_u64());

    recording.publish_resources();
    tick_until_drawn(cx, &page, recording.last_second());

    cx.update(|cx| {
        let page = page.read(cx);
        assert!(page.reader_rested());
        assert_eq!(
            page.snapshot.as_ref().unwrap().selection["second"].as_u64(),
            read
        );
        assert!(page.resident_history().is_some());
    });
    drop(recording);
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

/// Ticks `page` until it draws `second` from the store's View.
fn tick_until_drawn(
    cx: &mut gpui::TestAppContext,
    page: &gpui::Entity<LiveDashboard>,
    second: u64,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    for _ in 0..4_000 {
        page.update(cx, LiveDashboard::tick);
        let drawn = cx.update(|cx| page.read(cx).shown_second());
        if drawn == Some(second) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the page never drew {second}: {drawn:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the page never drew {second}");
}

#[gpui::test]
fn a_cpu_page_hidden_with_a_reading_on_its_way_keeps_its_chart_by_app(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: a reading the worker built while the View drew the chart, which the page takes
    // after macOS hid the window and the page let go of the View, shows the chart drawn from
    // the View with its history, and so does the window shown again before the store's next
    // View. Method: the CPU page over a real store and reader settles on the View, the
    // recording publishes a second, the worker reads it, macOS hides the window, and the page
    // ticks, then macOS shows the window and the page ticks again.
    let (mut recording, page) = followed_page_read_again(cx, "cpu");
    recording.publish_resources();
    let second = recording.last_second();
    wait_for_worker_reading(cx, &page, second);

    page.update(cx, |page, cx| page.observe_visibility_callback(false, cx));
    page.update(cx, LiveDashboard::tick);

    cx.update(|cx| {
        let page = page.read(cx);
        let snapshot = page.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.selection["second"].as_u64(), Some(second));
        assert!(page.store_view.is_none());
        assert!(by_app_holds(&page.panels, Metric::Cpu.panel_id()));
        assert!(page.attribution_history(Metric::Cpu).is_some());
    });
    page.update(cx, |page, cx| page.observe_visibility_callback(true, cx));
    page.update(cx, LiveDashboard::tick);
    cx.update(|cx| {
        let page = page.read(cx);
        assert!(by_app_holds(&page.panels, Metric::Cpu.panel_id()));
        assert!(page.attribution_history(Metric::Cpu).is_some());
    });
    drop(recording);
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

#[gpui::test]
fn a_second_inspected_on_a_cpu_page_drawn_from_the_view_reads_its_chart_until_the_reply(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: a second chosen without a hold, as an arrow step past the readings or the
    // automation's inspect chooses one, keeps the chart's readouts on the history the View
    // drew until the reading of that second replaces them. Method: the CPU page over a real
    // store and reader settles on the View and the worker's reading, then inspects the second
    // before the newest.
    let (recording, page) = followed_page_read_again(cx, "cpu");
    let second = recording.last_second() - 1;

    page.update(cx, |page, cx| page.inspect(second, cx));

    cx.update(|cx| {
        let page = page.read(cx);
        assert_eq!(page.request.second, Some(second));
        assert!(page.awaiting_reply());
        assert!(page.attribution_history(Metric::Cpu).is_some());
    });
    drop(recording);
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

/// Waits, without ticking `page`, until its worker holds a reading of `second` for the page.
fn wait_for_worker_reading(
    cx: &mut gpui::TestAppContext,
    page: &gpui::Entity<LiveDashboard>,
    second: u64,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    for _ in 0..4_000 {
        let read = cx.update(|cx| page.read(cx).worker.output_second());
        if read == Some(second) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the worker never read {second}: {read:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the worker never read {second}");
}

/// Whether `panels` hold the chart by App of id `id`.
fn by_app_holds(panels: &[Rc<Panel>], id: &str) -> bool {
    by_app(panels).iter().any(|(drawn, _, _)| drawn == id)
}

/// The page `route` over a real store and reader following a live recording, settled on the
/// store's View of its second publication and then on the worker's reading of a third.
fn followed_page_read_again(
    cx: &mut gpui::TestAppContext,
    route: &str,
) -> (
    crate::live_dashboard::input_tests::source_mode::LiveRecording,
    gpui::Entity<LiveDashboard>,
) {
    let (mut recording, page) = followed_page_settled(cx, route);

    recording.publish_resources();
    tick_until_read(cx, &page, recording.last_second());
    (recording, page)
}

/// The page `route` over a real store and reader following a live recording, settled on the
/// store's View of its second publication.
fn followed_page_settled(
    cx: &mut gpui::TestAppContext,
    route: &str,
) -> (
    crate::live_dashboard::input_tests::source_mode::LiveRecording,
    gpui::Entity<LiveDashboard>,
) {
    use gpui::AppContext;
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish_resources();
    recording.publish_resources();
    let (store, handle) = crate::live_source::store_start(&recording.root).unwrap();
    let worker = Worker::start_from_store(recording.root.clone(), handle).unwrap();
    cx.update(|cx| super::install_store(store, cx));
    let page = cx.update(|cx| cx.new(|_| LiveDashboard::new(worker, route.into(), None)));

    tick_until_settled(cx, &page, 2);
    (recording, page)
}

/// Ticks `page` until it shows the worker's reading of `second` with nothing more to come and
/// draws from the store's View of that second.
fn tick_until_read(cx: &mut gpui::TestAppContext, page: &gpui::Entity<LiveDashboard>, second: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    for _ in 0..4_000 {
        page.update(cx, LiveDashboard::tick);
        let (read, drawn, awaiting) = cx.update(|cx| {
            let page = page.read(cx);
            let read = page
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.selection["second"].as_u64());
            let drawn = page
                .store_view
                .as_ref()
                .and_then(|view| view.seconds.last())
                .map(|newest| newest.wall_ns / super::NANOSECONDS_PER_SECOND);
            (read, drawn, page.awaiting_reply())
        });
        if read == Some(second) && drawn == Some(second) && !awaiting {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the page never read {second}: {read:?}, {drawn:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the page never read {second}");
}

#[gpui::test]
fn a_second_held_on_overview_keeps_the_view_while_the_store_goes_on(cx: &mut gpui::TestAppContext) {
    // Goal: while a second is held on Overview the page keeps the View its charts were drawn
    // from, so nothing but letting go moves the charts, and the store goes on, since the pages of
    // the held second read it. Method: Overview drawn from a real store and read by a real
    // reader, a pin of the newest second the page draws, a second published, ticks of the page,
    // then the View the page holds and the store's activity.
    use gpui::AppContext;
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let (store, handle) = crate::live_source::store_start(&recording.root).unwrap();
    let worker = Worker::start_from_store(recording.root.clone(), handle).unwrap();
    cx.update(|cx| super::install_store(store, cx));
    let page = cx.update(|cx| cx.new(|_| LiveDashboard::new(worker, "overview".into(), None)));
    tick_until_settled(cx, &page, 1);
    let drawn = cx.update(|cx| page.read(cx).store_view.clone().unwrap());
    let second = cx.update(|cx| page.read(cx).store_view_last_second().unwrap());
    page.update(cx, |page, cx| page.pin_second(second, cx));
    recording.publish();

    for _ in 0..3 {
        page.update(cx, LiveDashboard::tick);
    }

    let kept = cx.update(|cx| page.read(cx).store_view.clone().unwrap());
    let newer = newer_store_view(cx, drawn.version);

    assert!(Arc::ptr_eq(&kept, &drawn));
    assert!(newer.version > drawn.version);
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

#[gpui::test]
fn overview_opened_again_draws_the_newest_second_the_store_read_on_another_page(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: Overview opened again draws the store's newest second at once, waiting on no
    // catch-up, no tick and no reader.
    // Method: leave an Overview drawn from a real store and read by a real reader for CPU,
    // publish a second while CPU shows and tick CPU past its handover, then open Overview and
    // read its chart untouched.
    use gpui::AppContext;
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let (store, handle) = crate::live_source::store_start(&recording.root).unwrap();
    let worker = Worker::start_from_store(recording.root.clone(), handle).unwrap();
    cx.update(|cx| super::install_store(store, cx));
    let page = cx.update(|cx| cx.new(|_| LiveDashboard::new(worker, "overview".into(), None)));
    tick_until_settled(cx, &page, 1);
    page.update(cx, |page, cx| page.navigate("cpu", cx));
    recording.publish();
    tick_until_settled(cx, &page, 2);
    page.update(cx, LiveDashboard::tick);

    page.update(cx, |page, cx| page.navigate("overview", cx));

    let window = cx.update(|cx| drawn(page.read(cx), "live-cpu").window.unwrap());
    assert_eq!(window.first + window.span - 1, 1001);
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

#[gpui::test]
fn a_live_window_that_opens_a_saved_recording_parks_the_store(cx: &mut gpui::TestAppContext) {
    // Goal: the live store stops ingesting once the window shows a saved recording, though it
    // stays installed for the live Mac.
    // Method: on a live CPU page over an installed live store, open the recording itself as a
    // saved one through a real reader, and tick until its first reading is accepted.
    use gpui::AppContext;
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let store = recording.start_store();
    cx.update(|cx| super::install_store(store, cx));
    let page = cx.update(|cx| cx.new(|_| opening_on("cpu")));
    page.update(cx, LiveDashboard::tick);
    assert!(cx.update(|cx| cx.global::<super::LiveStore>().0.is_active()));
    page.update(cx, |page, _| page.begin_recording(recording.root.clone()));

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    for _ in 0..2_000 {
        page.update(cx, LiveDashboard::tick);
        if cx.update(|cx| page.read(cx).native.source)
            == crate::live_source_mode::SourceMode::Recorded
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the recording never opened"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    assert!(!cx.update(|cx| cx.global::<super::LiveStore>().0.is_active()));
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

#[gpui::test]
fn overview_opened_with_no_saved_page_draws_the_view_the_store_kept_current(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: an Overview that no page of its own was kept for still draws the store's newest
    // second at once, waiting on no tick and no reader.
    // Method: open a real store and reader on CPU, tick until CPU holds the store's View, then
    // open Overview and read its chart untouched.
    use gpui::AppContext;
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let (store, handle) = crate::live_source::store_start(&recording.root).unwrap();
    let worker = Worker::start_from_store(recording.root.clone(), handle).unwrap();
    cx.update(|cx| super::install_store(store, cx));
    let page = cx.update(|cx| cx.new(|_| LiveDashboard::new(worker, "cpu".into(), None)));
    tick_until_settled(cx, &page, 1);

    page.update(cx, |page, cx| page.navigate("overview", cx));

    let window = cx.update(|cx| drawn(page.read(cx), "live-cpu").window.unwrap());
    assert_eq!(window.first + window.span - 1, 1000);
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

/// Ticks `page` until its reader has answered and it holds the store's View of `publication`,
/// as each quarter-second tick takes the store's newest View on whichever page shows.
fn tick_until_settled(
    cx: &mut gpui::TestAppContext,
    page: &gpui::Entity<LiveDashboard>,
    publication: u64,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    for _ in 0..2_000 {
        page.update(cx, LiveDashboard::tick);
        let held = cx.update(|cx| {
            let view = page.read(cx).store_view.clone()?;
            view.seconds.last().map(|second| second.publication)
        });
        if held == Some(publication) {
            if !cx.update(|cx| page.read(cx).awaiting_reply()) {
                return;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the page never settled on the store's View"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the page never settled on the store's View");
}

#[gpui::test]
fn a_resting_overview_polls_the_installed_store_until_its_view_arrives(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: the real store's pending handoff reaches Overview's fast-poll decision.
    // Method: pause a store behind a second publication and tick a rested page that waits for
    // the store's wake. The tick resumes the store and takes no View, so the handoff stays
    // pending however quick the store is. Then wake the page for the View.
    use gpui::AppContext;
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    recording.publish();
    let store = recording.start_store();
    store.set_active(false);
    cx.update(|cx| super::install_store(store, cx));
    wait_for_store_activity(cx, true);
    let stale = MachineSecond {
        publication: 1,
        ..reading_at(1000, 0)
    };
    let page = cx.update(|cx| {
        cx.new(|cx| {
            let mut page = opening();
            let mut snapshot = readings(std::slice::from_ref(&stale));
            snapshot.generation = page.generation;
            assert!(page.receive(Update::Ready(snapshot)));
            // The test wakes the page itself, after the tick it asserts on.
            page.wake_with(Box::new(|| {}), cx);
            assert!(page.take_view(Arc::new(view(1, &[], &[stale]))));
            // As the tick that took that View leaves the page, waiting for the store's wake.
            page.mailbox_watch = page.mailbox_watch.after_view();
            page.tick_rest();
            assert!(page.reader_rested());
            page
        })
    });

    page.update(cx, LiveDashboard::tick);

    assert!(cx.update(|cx| page.read(cx).awaiting_reply()));
    page.update(cx, LiveDashboard::wake_from_store);
    tick_until_settled(cx, &page, 2);
    cx.update(|cx| drop(cx.remove_global::<super::LiveStore>()));
}

mod when_the_store_wakes_the_page {
    use super::*;
    use gpui::AppContext;

    #[gpui::test]
    fn a_tick_before_the_wake_takes_no_view(cx: &mut gpui::TestAppContext) {
        // Goal: a page the store wakes asks it for nothing at its timer's ticks, so the page
        // looks for a View no more often than the store publishes. Method: a real store leaves a
        // second View for a page waiting for wakes, and the page ticks without being woken.
        let (mut recording, page) = page_waiting_for_wakes(cx);
        recording.publish();
        wait_for_a_view_left(cx);

        for _ in 0..3 {
            page.update(cx, LiveDashboard::tick);
        }

        assert_eq!(drawn_publication(cx, &page), Some(1));
        cx.update(|cx| drop(cx.remove_global::<super::super::LiveStore>()));
    }

    #[gpui::test]
    fn the_wake_takes_the_view_the_store_left(cx: &mut gpui::TestAppContext) {
        // Goal: a tick after the store's mark takes the View the store left. Method: a real store
        // leaves a second View for a page waiting for wakes, which is then marked and ticks. The
        // test marks the page and ticks it itself rather than through the store's wake, which
        // ticks only where no reading is on its way, and whether one is on this page's way
        // depends on how far the real worker has got.
        let (mut recording, page) = page_waiting_for_wakes(cx);
        recording.publish();
        wait_for_a_view_left(cx);

        page.update(cx, |page, cx| {
            page.mark_woken();
            page.tick(cx);
        });

        assert_eq!(drawn_publication(cx, &page), Some(2));
        cx.update(|cx| drop(cx.remove_global::<super::super::LiveStore>()));
    }

    #[gpui::test]
    fn a_view_woken_for_during_a_hold_is_taken_at_the_first_tick_after_it(
        cx: &mut gpui::TestAppContext,
    ) {
        // Goal: a hold delays the View a wake announced only until the hold ends, not until the
        // store's next publication. Method: a page waiting for wakes holds its newest second, is
        // woken for a second View, lets go, and ticks.
        let (mut recording, page) = page_waiting_for_wakes(cx);
        let second = cx.update(|cx| page.read(cx).store_view_last_second().unwrap());
        page.update(cx, |page, cx| page.pin_second(second, cx));
        recording.publish();
        wait_for_a_view_left(cx);
        page.update(cx, LiveDashboard::wake_from_store);

        page.update(cx, LiveDashboard::select);
        page.update(cx, LiveDashboard::tick);

        assert_eq!(drawn_publication(cx, &page), Some(2));
        cx.update(|cx| drop(cx.remove_global::<super::super::LiveStore>()));
    }

    fn page_waiting_for_wakes(
        cx: &mut gpui::TestAppContext,
    ) -> (
        crate::live_dashboard::input_tests::source_mode::LiveRecording,
        gpui::Entity<LiveDashboard>,
    ) {
        let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
        recording.publish();
        let (store, handle) = crate::live_source::store_start(&recording.root).unwrap();
        let worker = Worker::start_from_store(recording.root.clone(), handle).unwrap();
        cx.update(|cx| super::super::install_store(store, cx));
        let page = cx.update(|cx| {
            cx.new(|cx| {
                let mut page = LiveDashboard::new(worker, "overview".into(), None);
                // The test wakes the page itself, at the moment each test chooses.
                page.wake_with(Box::new(|| {}), cx);
                page
            })
        });
        tick_until_settled(cx, &page, 1);
        (recording, page)
    }

    fn wait_for_a_view_left(cx: &mut gpui::TestAppContext) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        for _ in 0..2_000 {
            if cx.update(|cx| super::super::store_pending(cx)) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the store left no View"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the store left no View");
    }

    fn drawn_publication(
        cx: &mut gpui::TestAppContext,
        page: &gpui::Entity<LiveDashboard>,
    ) -> Option<u64> {
        cx.update(|cx| {
            let view = page.read(cx).store_view.clone()?;
            view.seconds.last().map(|second| second.publication)
        })
    }
}

/// The Store's next View newer than `version`, which it hands over whatever View a page keeps.
fn newer_store_view(cx: &mut gpui::TestAppContext, version: u64) -> Arc<View> {
    for _attempt in 0..2_000 {
        let taken = cx.update(|cx| super::take_store_view(cx));
        if let Some(view) = taken.filter(|view| view.version > version) {
            return view;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the Store handed over no newer View");
}

mod when_the_store_or_the_worker_wakes_the_page {
    use std::sync::atomic::{AtomicBool, Ordering};

    use gpui::{AppContext, TestAppContext, VisualTestContext, WindowOptions};

    use super::*;
    use crate::live_dashboard::input_tests::close_dashboard_window;
    use crate::live_dashboard::input_tests::source_mode::LiveRecording;
    use crate::live_source::measurement::{Hold, Stage};

    #[gpui::test]
    fn the_workers_wake_draws_the_view_with_the_reading(cx: &mut TestAppContext) {
        // Goal: a followed page draws a second once, with the store's View and the worker's
        // reading of it together, since the reading is on its way when the store wakes the page.
        // Method: hold the worker at the start of a step, publish a second, wake the page as the
        // store does, let the worker read the second, wake the page as the worker does, and count
        // the page's draws.
        let mut followed = Followed::open(cx, "cpu");
        followed.wait_for_a_keyed_question();
        let drawn = followed.renders();
        followed.publish_with_the_worker_held();

        followed.store_wakes();
        followed.worker_reads();
        followed.worker_wakes();

        assert_eq!(followed.renders(), drawn + 1);
        assert_eq!(followed.reading_second(), followed.view_second());
    }

    #[gpui::test]
    fn the_stores_wake_takes_the_view_at_once_where_no_reading_is_on_its_way(
        cx: &mut TestAppContext,
    ) {
        // Goal: where no reading is on its way, as at launch and on the Self page, which shows
        // none, the store's wake takes its View at once, since no worker's wake brings it.
        // Method: publish a second to the Self page and wake the page as the store does. The
        // Self page draws nothing from a View, so the test asserts the View it took rather than
        // a draw.
        let mut followed = Followed::open(cx, "self");
        let published = followed.publish();

        followed.store_wakes();

        assert_eq!(followed.view_publication(), Some(published));
    }

    #[gpui::test]
    fn the_workers_wake_that_runs_first_takes_the_view_with_the_reading(cx: &mut TestAppContext) {
        // Goal: the run loop may run the worker's wake before the store's, and the page still
        // draws the second once, since the worker's tick takes the View too and the store's wake
        // after it finds nothing new. Method: hold the worker, publish a second, let the worker
        // read it, wake the page as the worker does and then as the store does, and count the
        // page's draws.
        let mut followed = Followed::open(cx, "cpu");
        followed.wait_for_a_keyed_question();
        let drawn = followed.renders();
        followed.publish_with_the_worker_held();
        followed.worker_reads();

        followed.worker_wakes();
        followed.store_wakes();

        assert_eq!(followed.renders(), drawn + 1);
        assert_eq!(followed.reading_second(), followed.view_second());
    }

    #[gpui::test]
    fn the_timers_tick_takes_a_view_no_reading_came_with(cx: &mut TestAppContext) {
        // Goal: a View whose reading never comes waits no longer than the page's next tick.
        // Method: hold the worker so that its reading does not come, publish a second, wake the
        // page as the store does, which leaves the View to the reading, and tick it as its timer
        // does, and count the page's draws.
        let mut followed = Followed::open(cx, "cpu");
        followed.wait_for_a_keyed_question();
        let earlier = followed.view_publication();
        let drawn = followed.renders();
        let published = followed.publish_with_the_worker_held();
        followed.store_wakes();
        assert_eq!(followed.view_publication(), earlier);
        assert_eq!(followed.renders(), drawn);

        followed.timer_ticks();

        assert_eq!(followed.view_publication(), Some(published));
        assert_eq!(followed.renders(), drawn + 1);
    }

    #[gpui::test]
    fn a_held_second_keeps_its_view_across_both_wakes(cx: &mut TestAppContext) {
        // Goal: a hold keeps the View its charts were drawn from, whichever wake comes. Method:
        // hold the page's newest second, publish another, and wake the page as the store does and
        // as the worker does.
        let mut followed = Followed::open(cx, "memory");
        followed.hold_the_newest_second();
        let held = followed.view_publication();
        followed.publish();

        followed.store_wakes();
        followed.worker_wakes();

        assert_eq!(followed.view_publication(), held);
    }

    #[gpui::test]
    fn the_workers_wake_with_no_view_left_has_the_timer_ask_the_store_for_nothing(
        cx: &mut TestAppContext,
    ) {
        // Goal: a page waiting for the store's wake asks the store's mailbox for nothing at its
        // timer's ticks, also after a worker's wake that came with no View, as for a hover's
        // reading. Method: open a followed page, which the store has left no View, wake it as
        // the worker does, tick it as its timer does, and ask whether its ticks look for a View.
        let mut followed = Followed::open(cx, "memory");
        assert!(!followed.store_pending());

        followed.worker_wakes();
        followed.timer_ticks();

        assert!(!followed.ticks_look_for_a_view());
    }

    #[gpui::test]
    fn a_page_of_an_app_with_no_store_takes_the_workers_wake(cx: &mut TestAppContext) {
        // Goal: the worker's wake leaves a page that looks for the store's View at every tick as
        // it is, since no store wakes it, and does not panic there. Method: open a page in a test
        // window with no store installed, have it wait for wakes as the app's page does, and wake
        // it as the worker does.
        let window = cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |_, cx| {
                cx.new(|cx| {
                    let mut page = opening();
                    page.wake_with(Box::new(|| {}), cx);
                    page
                })
            })
            .unwrap()
        });
        let page = window.root(cx).unwrap();
        let mut window = VisualTestContext::from_window(window.into(), cx);

        page.update(&mut window, LiveDashboard::wake_from_worker);

        close_dashboard_window(&mut window, &page);
    }

    mod when_one_tick_takes_the_reading_and_the_view {
        use super::*;

        #[gpui::test]
        fn the_page_builds_its_charts_by_app_once(cx: &mut TestAppContext) {
            // Goal: a followed page that reads each second builds its charts by App once in a tick
            // that takes the worker's reading and the store's View of one second, rather than
            // from the earlier View for the reading and again from the new one. Method: hold the
            // worker, publish a second, wake the page as the store does, let the worker read the
            // second, wake the page as the worker does, and count the page's builds.
            let mut followed = Followed::open(cx, "cpu");
            followed.wait_for_a_keyed_question();
            let built = followed.builds();
            followed.publish_with_the_worker_held();
            followed.store_wakes();
            followed.worker_reads();

            followed.worker_wakes();

            assert_eq!(followed.builds(), built + 1);
            assert_eq!(followed.reading_second(), followed.view_second());
        }
    }

    mod when_the_tick_looks_ahead_of_the_pages_reading {
        use super::*;
        use crate::live_dashboard::Presence;
        use crate::live_dashboard::store_view::MailboxWatch;

        #[gpui::test]
        fn it_takes_the_view_the_store_left(cx: &mut TestAppContext) {
            // Goal: a followed page shown, waking for the store's View, takes that View ahead of
            // its reading. Method: publish a second to a followed page and look ahead of a
            // reading of the page, changing nothing.
            let mut followed = Followed::open(cx, "memory");
            let published = followed.publish();

            assert!(followed.takes_the_view_ahead(|_| {}));
            assert_eq!(followed.view_publication(), Some(published));
        }

        #[gpui::test]
        fn a_page_whose_reader_rests_takes_the_view(cx: &mut TestAppContext) {
            // Goal: a page whose reader rests is shown, and a reading wakes its reader, so it
            // takes the View ahead as a page shown does. Method: as in the first test, with
            // the reader resting.
            let mut followed = Followed::open(cx, "memory");
            let published = followed.publish();

            assert!(followed.takes_the_view_ahead(|page| page.presence = Presence::Rested));
            assert_eq!(followed.view_publication(), Some(published));
        }

        #[gpui::test]
        fn a_held_second_leaves_the_view_to_a_later_tick(cx: &mut TestAppContext) {
            // Goal: a held second keeps the View its charts were drawn from, as the tick's own
            // look leaves it. Method: as in the first test, holding the newest second first.
            let mut followed = Followed::open(cx, "memory");
            followed.hold_the_newest_second();
            followed.publish();

            assert!(!followed.takes_the_view_ahead(|_| {}));
            assert!(followed.store_pending());
        }

        #[gpui::test]
        fn a_hover_leaves_the_view_to_the_ticks_look(cx: &mut TestAppContext) {
            // Goal: a hover, which takes the page's reading itself and may end within the tick,
            // leaves the View to the tick's own look. Method: as in the first test, with a hover
            // shown.
            let mut followed = Followed::open(cx, "memory");
            followed.publish();

            let ahead = followed.takes_the_view_ahead(|page| page.preview = Some(preview(page)));

            assert!(!ahead);
            assert!(followed.store_pending());
        }

        #[gpui::test]
        fn a_window_macos_hides_leaves_the_view(cx: &mut TestAppContext) {
            // Goal: a window that hides in the tick, or is hidden, takes no View, and neither
            // does one macOS hid from the start, which hides at its first reading. Method: as
            // in the first test, with the window opening hidden, hiding and then hidden.
            let mut followed = Followed::open(cx, "memory");
            followed.publish();

            assert!(!followed.takes_the_view_ahead(|page| page.presence = Presence::Opening));
            assert!(!followed.takes_the_view_ahead(|page| page.presence = Presence::Hiding));
            assert!(!followed.takes_the_view_ahead(|page| page.presence = Presence::Hidden));
            assert!(followed.store_pending());
        }

        #[gpui::test]
        fn a_window_shown_again_leaves_the_view_to_its_own_look(cx: &mut TestAppContext) {
            // Goal: a window shown again takes its View as it shows, so the tick does not take
            // it ahead. Method: as in the first test, with the window showing again.
            let mut followed = Followed::open(cx, "memory");
            followed.publish();

            assert!(!followed.takes_the_view_ahead(|page| page.presence = Presence::Showing));
            assert!(followed.store_pending());
        }

        #[gpui::test]
        fn a_page_waiting_for_the_stores_wake_leaves_the_view(cx: &mut TestAppContext) {
            // Goal: a page whose tick would not look in the mailbox does not look ahead either.
            // Method: as in the first test, on a page the store has not woken.
            let mut followed = Followed::open(cx, "memory");
            followed.publish();

            let ahead = followed.takes_the_view_ahead(|page| {
                page.mailbox_watch = MailboxWatch::FromWake { woken: false };
            });

            assert!(!ahead);
            assert!(followed.store_pending());
        }

        #[gpui::test]
        fn a_page_of_a_chosen_second_leaves_the_view(cx: &mut TestAppContext) {
            // Goal: a page that no longer follows the newest second, which lets go of its View
            // within the tick, takes none ahead. Method: as in the first test, on a page whose
            // request names a second.
            let mut followed = Followed::open(cx, "memory");
            followed.publish();

            let ahead = followed.takes_the_view_ahead(|page| page.request.second = Some(10_000));

            assert!(!ahead);
            assert!(followed.store_pending());
        }

        #[gpui::test]
        fn a_reading_of_an_earlier_request_leaves_the_view(cx: &mut TestAppContext) {
            // Goal: a reading the page will not take draws nothing, so the View waits for the
            // tick's own look. Method: as in the first test, with the page asking anew.
            let mut followed = Followed::open(cx, "memory");
            followed.publish();

            assert!(!followed.takes_the_view_ahead(|page| page.generation += 1));
            assert!(followed.store_pending());
        }
    }

    /// A page following the newest second of a real store and worker in a test window, which the
    /// test wakes by hand where the app's two wakes would, in the order each test chooses.
    struct Followed {
        recording: LiveRecording,
        page: gpui::Entity<LiveDashboard>,
        window: VisualTestContext,
        /// Whether the worker has woken the page since the test last looked.
        worker_woke: Arc<AtomicBool>,
        /// The hold on the worker at the start of a step, which `worker_reads` lets go of, and
        /// dropping the page does where no test did.
        held_worker: Option<Hold>,
    }

    impl Followed {
        /// Opens the page of `route` in a test window over a recording of one second. It builds
        /// its own page rather than through `page_waiting_for_wakes`, which opens Overview with
        /// no window, since the tests count the page's draws and choose its route.
        fn open(cx: &mut TestAppContext, route: &str) -> Self {
            let mut recording = LiveRecording::new();
            recording.publish();
            let (store, handle) = crate::live_source::store_start(&recording.root).unwrap();
            let worker = Worker::start_from_store(recording.root.clone(), handle).unwrap();
            let worker_woke = Arc::new(AtomicBool::new(false));
            let signal = Arc::clone(&worker_woke);
            worker.wake_page_with(Box::new(move || signal.store(true, Ordering::Release)));
            cx.update(|cx| super::super::install_store(store, cx));
            let window = cx.update(|cx| {
                cx.open_window(WindowOptions::default(), |_, cx| {
                    cx.new(|cx| {
                        let mut page = LiveDashboard::new(worker, route.into(), None);
                        // The test wakes the page itself, at the moment each test chooses.
                        page.wake_with(Box::new(|| {}), cx);
                        page
                    })
                })
                .unwrap()
            });
            let page = window.root(cx).unwrap();
            let mut window = VisualTestContext::from_window(window.into(), cx);
            tick_until_settled(&mut window, &page, recording.publication());
            window.run_until_parked();

            Self {
                recording,
                page,
                window,
                worker_woke,
                held_worker: None,
            }
        }

        /// Waits until the worker has its keyed question out, which the store answers once it
        /// takes the next second.
        fn wait_for_a_keyed_question(&self) {
            for _attempt in 0..2_000 {
                if self.reading_on_its_way() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            panic!("the worker put no keyed question out");
        }

        fn reading_on_its_way(&self) -> bool {
            self.page
                .read_with(&self.window, |page, _| page.worker.reading_on_its_way())
        }

        fn store_pending(&self) -> bool {
            TestAppContext::update(&self.window, |cx| super::super::store_pending(cx))
        }

        fn ticks_look_for_a_view(&self) -> bool {
            self.page
                .read_with(&self.window, |page, _| page.mailbox_watch.looks())
        }

        fn renders(&self) -> u64 {
            self.page.read_with(&self.window, |page, _| page.renders)
        }

        fn builds(&self) -> u64 {
            self.page
                .read_with(&self.window, |page, _| page.store_panels.builds)
        }

        fn view_second(&self) -> Option<u64> {
            self.page.read_with(&self.window, |page, _| {
                let second = page.store_view.as_ref()?.seconds.last()?;
                Some(second.wall_ns / super::super::NANOSECONDS_PER_SECOND)
            })
        }

        fn view_publication(&self) -> Option<u64> {
            self.page.read_with(&self.window, |page, _| {
                Some(page.store_view.as_ref()?.seconds.last()?.publication)
            })
        }

        fn reading_second(&self) -> Option<u64> {
            self.page.read_with(&self.window, |page, _| {
                page.snapshot.as_ref()?.selection["second"].as_u64()
            })
        }

        /// Holds the worker at the start of its next step, then publishes a second, waits until
        /// the store has left its View of it, and answers that second's publication.
        fn publish_with_the_worker_held(&mut self) -> u64 {
            assert!(self.held_worker.is_none());
            let hold = self.page.read_with(&self.window, |page, _| {
                page.worker.hold_measurement_stage(Stage::Reading)
            });
            hold.entered();
            self.held_worker = Some(hold);

            self.publish()
        }

        /// Publishes a second, waits until the store has left its View of it, and answers that
        /// second's publication.
        fn publish(&mut self) -> u64 {
            self.worker_woke.store(false, Ordering::Release);
            self.recording.publish();
            for _attempt in 0..2_000 {
                if self.store_pending() {
                    return self.recording.publication();
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            panic!("the store left no View");
        }

        /// Lets the held worker read the second, and waits until it has woken the page.
        fn worker_reads(&mut self) {
            let Some(hold) = self.held_worker.take() else {
                panic!("the worker is not held");
            };
            hold.release();
            for _attempt in 0..2_000 {
                if self.worker_woke.swap(false, Ordering::AcqRel) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            panic!("the worker did not wake the page");
        }

        /// Holds the newest second the page shows, as a pick on a chart does.
        fn hold_the_newest_second(&mut self) {
            self.page.update(&mut self.window, |page, cx| {
                let second = page.shown_second().unwrap();
                page.pin_second(second, cx);
            });
            self.window.run_until_parked();
        }

        /// Looks in the store's mailbox ahead of a reading of the page's request, as a tick does,
        /// on the page woken for the store's View and then changed by `change`, and answers
        /// whether it took the View.
        fn takes_the_view_ahead(&mut self, change: impl FnOnce(&mut LiveDashboard)) -> bool {
            self.page.update(&mut self.window, |page, cx| {
                let mut reading = readings(&[reading_at(10_000, 0)]);
                reading.generation = page.generation;
                page.mark_woken();
                change(page);
                page.tick_view_ahead(&Ok(Some(Update::Ready(reading))), cx)
            })
        }

        fn timer_ticks(&mut self) {
            self.page.update(&mut self.window, LiveDashboard::tick);
            self.window.run_until_parked();
        }

        fn store_wakes(&mut self) {
            self.page
                .update(&mut self.window, LiveDashboard::wake_from_store);
            self.window.run_until_parked();
        }

        fn worker_wakes(&mut self) {
            self.page
                .update(&mut self.window, LiveDashboard::wake_from_worker);
            self.window.run_until_parked();
        }
    }

    impl Drop for Followed {
        fn drop(&mut self) {
            // The worker stops as the window closes, so it must not wait at a hold.
            drop(self.held_worker.take());
            close_dashboard_window(&mut self.window, &self.page);
            // As in `close_dashboard_window`, a test that failed inside an update leaves its
            // window taken, so updating it here would panic again during the unwind, which aborts
            // every test still to run.
            if std::thread::panicking() {
                return;
            }
            TestAppContext::update(&self.window, |cx| {
                drop(cx.remove_global::<super::super::LiveStore>());
            });
        }
    }
}
