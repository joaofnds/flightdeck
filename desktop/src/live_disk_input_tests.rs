use std::sync::Arc;

use gpui::{TestAppContext, px};
use serde_json::json;

use super::{InputDriver, InputHarness, PlotExit, accepted_snapshot};
use crate::live_rankings::Rankings;
use crate::live_source::Snapshot;

#[test]
fn disk_ranking_preserves_jointly_measured_interval_bytes_and_rate() {
    // Fold a two-second interval so the combined exact byte amount differs from its rate.
    let snapshot = accepted_snapshot(1);

    let rankings = disk_rankings(&snapshot);

    let Rankings::Ready(rankings) = rankings else {
        panic!("{rankings:?}");
    };
    let disk = rankings
        .metrics
        .iter()
        .find(|ranking| ranking.metric.title() == "Disk I/O")
        .expect("Disk I/O ranking missing");
    assert_eq!(rankings.selected, snapshot.selection["selected"]);
    assert_eq!(disk.entries.len(), 1);
    assert_eq!(disk.entries[0].app.name, "Current");
    assert_eq!(disk.entries[0].amount, 3_000_000);
    assert_eq!(disk.entries[0].rate, Some(1_500_000.0));
    assert_eq!(disk.entries[0].measured_members, 1);
    assert_eq!(disk.entries[0].unavailable_members, 0);
}

#[gpui::test]
fn storage_disk_ranking_displays_the_selected_observation_interval(cx: &mut TestAppContext) {
    // Render the production Storage route from one bounded App page with a two-second rate.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.teardown();
}

#[gpui::test]
fn storage_hover_waits_for_exact_historical_app_details(cx: &mut TestAppContext) {
    // Dispatch through the installed Storage graph before any exact App preview is delivered.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();

    harness
        .driver
        .hover_previous_second_on("live-storage-throughput-plot");

    assert!(
        harness
            .driver
            .reading_is_visible("Hover preview · Loading exact details")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.teardown();
}

#[gpui::test]
fn storage_disk_hover_uses_exact_ranks_and_restores_live_on_exit(cx: &mut TestAppContext) {
    // Deliver a different historical disk total through the graph's actual hover request.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness
        .driver
        .hover_previous_second_on("live-storage-throughput-plot");

    harness.driver.complete_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Historical:20.0 B/s")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness
        .driver
        .leave_plot("live-storage-throughput-plot", PlotExit::Sidebar);
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Historical:20.0 B/s")
    );
    harness.teardown();
}

#[gpui::test]
fn storage_disk_link_keeps_its_keyboard_selected_observation(cx: &mut TestAppContext) {
    // Pause through the installed Storage chart key handler, then open the displayed App rank.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness
        .driver
        .inspect_ranked_history_with_keyboard_on("live-storage-throughput-plot");
    harness.driver.complete_ranked_selection();

    harness
        .driver
        .click_ranked_app("ranking:Disk I/O:Historical:20.0 B/s");

    harness.driver.assert_ranked_app_request(999, 1, "42");
    harness.teardown();
}

#[gpui::test]
fn storage_disk_reader_failure_never_exposes_live_links(cx: &mut TestAppContext) {
    // A failed exact historical worker read cannot substitute the current App's disk reading.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness
        .driver
        .hover_previous_second_on("live-storage-throughput-plot");

    harness.driver.fail_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: Ranking read failed")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.teardown();
}

#[gpui::test]
fn storage_disk_rank_failure_never_exposes_live_links(cx: &mut TestAppContext) {
    // Scalar history can arrive successfully while only its exact App totals fail.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    harness
        .driver
        .hover_previous_second_on("live-storage-throughput-plot");

    harness.driver.complete_unavailable_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: App response bound")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.teardown();
}

#[gpui::test]
fn storage_disk_gap_has_no_current_app_link(cx: &mut TestAppContext) {
    // A second without an observation has no App identity to use for disk attribution.
    let mut harness = InputHarness::setup_route(cx, "storage");
    harness.driver.seed_disk_storage();
    let position = harness
        .driver
        .second_position("live-storage-throughput-plot", 897.5 / 900.0);

    harness.driver.hover_position(position);
    harness.driver.finish_missing_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable at this observation")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:Disk I/O:Current:1.5 MB/s")
    );
    harness.teardown();
}

impl InputDriver {
    fn seed_disk_storage(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
            snapshot.rankings = Some(Arc::new(disk_rankings(&snapshot)));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(3500.0)));
        self.context.run_until_parked();
    }
}

fn disk_rankings(snapshot: &Snapshot) -> Rankings {
    let measure = |known: u64| {
        json!({"known":known.to_string(),"measured_members":1,
            "unavailable_members":0,"overflowed":false})
    };
    let app = json!({"display_name":"Current","key":{"kind":"bundle","bytes_hex":"41"},
        "cpu_ns":measure(1_000_000),"resident_bytes":measure(1024),
        "energy_nj":measure(1_000_000_000),"cpu_percent_one_core":0.05,"energy_watts":0.5,
        "disk_read_bytes":measure(2_000_000),"disk_written_bytes":measure(1_000_000),
        "disk_io_bytes":measure(3_000_000),"disk_read_bps":1_000_000.0,
        "disk_written_bps":500_000.0,"disk_io_bps":1_500_000.0});
    let page = json!({"selected":snapshot.selection["selected"],"elapsed_ns":"2000000000",
        "apps":[app],"app_count":1,"membership_complete":true,"ungrouped_members":0,
        "page":{"offset":0,"limit":512,"total":1,"next_offset":null}});

    crate::live_rankings::read(&snapshot.selection["selected"], |_| Ok(page.clone()))
}
