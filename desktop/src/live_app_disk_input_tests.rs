use gpui::Modifiers;
use num_traits::ToPrimitive;
use serde_json::{Value, json};

use super::{AppEpoch, AppHoverDriver, AppHoverHarness, wire_observation};
use crate::live_source::{Snapshot, Update};

#[gpui::test]
fn selected_app_disk_summary_and_columns_keep_joint_coverage(cx: &mut gpui::TestAppContext) {
    // Show exact two-second rates while excluding an Exec interval from final App/group totals.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_disk_interval();

    assert!(
        harness
            .driver
            .text_is_visible("summary-card:Disk I/O:1.5 MB/s *")
    );
    assert!(
        harness
            .driver
            .text_is_visible("app-disk:bundle:41:1.5 MB/s *")
    );
    assert!(
        harness
            .driver
            .text_is_visible("executable-disk:hex:61:1.5 MB/s *")
    );
    let executable = harness.driver.selected_disk_executable();
    assert_eq!(executable["scope"], "loaded process page only");
    assert_eq!(executable["disk_io_bytes"], disk_measure("3000000"));
    assert_eq!(executable["disk_read_bytes"], disk_measure("2000000"));
    assert_eq!(executable["disk_written_bytes"], disk_measure("1000000"));
    harness.teardown();
}

#[gpui::test]
fn disk_hover_replaces_every_app_widget_then_restores_the_committed_interval(
    cx: &mut gpui::TestAppContext,
) {
    // Dispatch actual graph input and deliver the exact generation with distinct disk totals.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_disk_interval();
    harness.driver.assert_current_disk_visible();
    harness.driver.assert_disk_history(
        1000,
        "1.5 MB/s · known subtotal · 2 measured · 1 unavailable",
        1,
        1.5,
    );

    harness.driver.hover_previous_second();

    harness.driver.assert_current_disk_hidden();
    harness.driver.complete_disk_history();
    harness.driver.assert_current_disk_hidden();
    harness.driver.assert_historical_disk_visible();
    harness.driver.assert_disk_history(999, "3 MB/s", 0, 3.0);
    harness.driver.leave_history();
    harness.driver.assert_current_disk_visible();
    harness.driver.assert_historical_disk_hidden();
    harness.driver.assert_disk_history(
        1000,
        "1.5 MB/s · known subtotal · 2 measured · 1 unavailable",
        1,
        1.5,
    );
    harness.driver.assert_disk_selection_unchanged();
    harness.teardown();
}

#[gpui::test]
fn failed_disk_hover_keeps_committed_app_widgets_hidden(cx: &mut gpui::TestAppContext) {
    // Refuse the pending exact observation after graph input instead of reusing current detail.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_disk_interval();
    harness.driver.assert_current_disk_visible();

    harness.driver.hover_previous_second();
    harness.driver.fail_disk_history();

    harness.driver.assert_current_disk_hidden();
    harness.driver.assert_historical_disk_hidden();
    harness.driver.leave_history();
    harness.driver.assert_current_disk_visible();
    harness.driver.assert_disk_selection_unchanged();
    harness.teardown();
}

#[gpui::test]
fn a_disk_history_gap_has_no_app_widgets_or_neighboring_readout(cx: &mut gpui::TestAppContext) {
    // Hover an unobserved second through the real graph while both neighboring epochs exist.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_disk_interval();
    harness.driver.assert_current_disk_visible();

    harness.driver.hover_disk_gap();

    harness.driver.assert_current_disk_hidden();
    harness.driver.assert_historical_disk_hidden();
    harness.driver.assert_disk_gap();
    harness.driver.leave_history();
    harness.driver.assert_current_disk_visible();
    harness.driver.assert_disk_selection_unchanged();
    harness.teardown();
}

#[gpui::test]
fn disk_history_small_rates_use_adaptive_headline_and_legend(cx: &mut gpui::TestAppContext) {
    // Goal: small combined rates read in the unit that suits them.
    // Method: render complete two-second byte deltas through production chart and accessibility
    // paths.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_disk_readout(DiskReadout::Small);

    harness.driver.assert_small_complete_disk_readout();
    harness.teardown();
}

#[gpui::test]
fn disk_history_zero_rates_remain_measured_in_headline_and_legend(cx: &mut gpui::TestAppContext) {
    // Goal: a measured zero stays distinct from unavailable.
    // Method: render measured zero deltas through the same real chart readouts.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_disk_readout(DiskReadout::Zero);

    harness.driver.assert_disk_readout(&DiskReadoutExpectation {
        second: 1000,
        headline: "0 B/s",
        headline_selector: "chart-reading:live-app-disk:0 B/s",
        legend: "chart-legend:live-app-disk:Example App:0 B/s",
    });
    harness.teardown();
}

#[gpui::test]
fn disk_history_unavailable_rates_stay_unavailable_in_all_readouts(cx: &mut gpui::TestAppContext) {
    // Goal: nullable rates with no measured members never read as a measured zero.
    // Method: render an interval no member measured through the real chart readouts.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_disk_readout(DiskReadout::Unavailable);

    harness.driver.assert_disk_readout(&DiskReadoutExpectation {
        second: 1000,
        headline: "Unavailable",
        headline_selector: "chart-reading:live-app-disk:Unavailable",
        legend: "chart-legend:live-app-disk:Example App:Unavailable",
    });
    harness.teardown();
}

#[gpui::test]
fn disk_history_coverage_follows_hover_and_exit(cx: &mut gpui::TestAppContext) {
    // Goal: the combined line's coverage follows the hovered publication and returns on exit.
    // Method: hover a partial second through the plot, deliver its exact details, then leave.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_disk_readout(DiskReadout::Complete);
    harness.driver.assert_complete_disk_readout();

    harness.driver.hover_previous_second();

    harness.driver.assert_partial_disk_readout();
    harness.driver.complete_disk_readout_history();
    harness.driver.assert_partial_disk_readout();
    harness.driver.leave_history();
    harness.driver.assert_complete_disk_readout();
    assert!(!harness.driver.text_is_visible(concat!(
        "chart-legend:live-app-disk:Example App:",
        "2.5 MB/s · known subtotal · 1 measured · 2 unavailable",
    )));
    harness.driver.assert_disk_selection_unchanged();
    harness.teardown();
}

#[gpui::test]
fn disk_headline_marks_partial_writes_with_complete_reads(cx: &mut gpui::TestAppContext) {
    // Goal: complete reads do not hide the members whose writes are unavailable.
    // Method: seed reads from every member and writes from one, then read the combined line.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_disk_readout(DiskReadout::WritePartial);

    harness.driver.assert_partial_small_disk_readout(1000);
    harness.teardown();
}

#[gpui::test]
fn disk_headline_marks_partial_reads_with_complete_writes(cx: &mut gpui::TestAppContext) {
    // Goal: complete writes do not hide the members whose reads are unavailable.
    // Method: seed writes from every member and reads from one, then read the combined line.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_disk_readout(DiskReadout::ReadPartial);

    harness.driver.assert_partial_small_disk_readout(1000);
    harness.teardown();
}

#[gpui::test]
fn disk_headline_is_unavailable_when_reads_are_unavailable(cx: &mut gpui::TestAppContext) {
    // Goal: known writes alone never stand in for the App's combined disk rate.
    // Method: seed writes from every member and reads from none, then read the combined line.
    let mut harness = AppHoverHarness::setup(cx);
    harness
        .driver
        .seed_disk_readout(DiskReadout::ReadUnavailable);

    harness.driver.assert_disk_readout(&DiskReadoutExpectation {
        second: 1000,
        headline: "Unavailable",
        headline_selector: "chart-reading:live-app-disk:Unavailable",
        legend: "chart-legend:live-app-disk:Example App:Unavailable",
    });
    harness.teardown();
}

#[gpui::test]
fn disk_headline_partial_follows_loading_exact_hover_and_exit(cx: &mut gpui::TestAppContext) {
    // Goal: equal rates isolate the coverage change across the exact hovered publication.
    // Method: hover a second whose writes are partial, deliver its details, then leave.
    let mut harness = AppHoverHarness::setup(cx);
    harness
        .driver
        .seed_disk_readout(DiskReadout::WritePartialHistory);
    harness.driver.assert_small_complete_disk_readout();

    harness.driver.hover_previous_second();

    harness.driver.assert_partial_small_disk_readout(999);
    harness.driver.complete_write_partial_disk_history();
    harness.driver.assert_partial_small_disk_readout(999);
    harness.driver.leave_history();
    harness.driver.assert_small_complete_disk_readout();
    assert!(!harness.driver.text_is_visible(concat!(
        "chart-legend:live-app-disk:Example App:",
        "2.6 kB/s · known subtotal · 1 measured · 2 unavailable",
    )));
    harness.driver.assert_disk_selection_unchanged();
    harness.teardown();
}

struct DiskReadoutExpectation {
    second: u64,
    headline: &'static str,
    headline_selector: &'static str,
    legend: &'static str,
}

#[gpui::test]
fn disk_plot_stays_stationary_during_partial_hover_at_narrow_width(cx: &mut gpui::TestAppContext) {
    // Hover the actual narrow disk plot and compare all four composed frames after coverage wraps.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_disk_readout(DiskReadout::Complete);
    harness.driver.reveal_narrow_disk_plot();
    let accepted = harness.driver.disk_plot_frame("complete");
    let plot = crate::charts::plot_bounds(accepted);
    let pointer = gpui::point(
        plot.left() + plot.size.width * (898.5 / 900.0),
        plot.center().y,
    );

    harness
        .driver
        .context
        .simulate_mouse_move(pointer, None, Modifiers::none());
    let loading = harness.driver.disk_plot_frame("partial-loading");
    harness
        .driver
        .view
        .read_with(&harness.driver.context, |dashboard, _| {
            let preview = dashboard.preview.as_ref().unwrap();
            assert_eq!(preview.source.as_ref(), "live-app-disk");
            assert_eq!(preview.target.second, 999);
        });
    harness.driver.complete_disk_readout_history();
    let exact = harness.driver.disk_plot_frame("partial-exact");
    harness.driver.context.simulate_mouse_move(
        gpui::point(gpui::px(20.0), pointer.y),
        None,
        Modifiers::none(),
    );
    let exited = harness.driver.disk_plot_frame("exit");

    eprintln!("QA_DISK_PLOT pointer={pointer:?}");
    assert_eq!(loading, accepted, "Partial loading moved the disk plot");
    assert_eq!(exact, accepted, "Exact details moved the disk plot");
    assert_eq!(exited, accepted, "Leaving hover moved the disk plot");
    harness.teardown();
}

impl AppHoverDriver {
    fn assert_small_complete_disk_readout(&mut self) {
        self.assert_disk_readout(&DiskReadoutExpectation {
            second: 1000,
            headline: "2.6 kB/s",
            headline_selector: "chart-reading:live-app-disk:2.6 kB/s",
            legend: "chart-legend:live-app-disk:Example App:2.6 kB/s",
        });
    }

    fn assert_partial_small_disk_readout(&mut self, second: u64) {
        self.assert_disk_readout(&DiskReadoutExpectation {
            second,
            headline: "2.6 kB/s · known subtotal · 1 measured · 2 unavailable",
            headline_selector: "chart-reading:live-app-disk:2.6 kB/s *",
            legend: concat!(
                "chart-legend:live-app-disk:Example App:",
                "2.6 kB/s · known subtotal · 1 measured · 2 unavailable",
            ),
        });
    }

    fn complete_write_partial_disk_history(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
            let preview = dashboard.preview.as_ref().unwrap();
            let generation = preview.generation.unwrap();
            let snapshot = disk_readout_snapshot(
                generation,
                AppEpoch::Historical,
                DiskReadout::WritePartialHistory,
            );
            assert_eq!(snapshot.generation, generation);
            assert_eq!(snapshot.selection["second"], preview.target.second);
            assert_eq!(
                Some(&snapshot.selection["selected"]),
                preview.target.identity.as_ref()
            );
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn reveal_narrow_disk_plot(&mut self) {
        self.context
            .simulate_resize(gpui::size(gpui::px(640.0), gpui::px(900.0)));
        self.context.run_until_parked();
        self.view.update(&mut self.context, |dashboard, cx| {
            let index = dashboard
                .rows
                .iter()
                .position(|row| {
                    matches!(row, crate::live_dashboard::Row::Charts { panels, .. }
                    if panels.iter().any(|panel| panel.id == "live-app-disk"))
                })
                .unwrap();
            dashboard.list.scroll_to(gpui::ListOffset {
                item_ix: index,
                offset_in_item: gpui::px(0.0),
            });
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn disk_plot_frame(&mut self, phase: &str) -> gpui::Bounds<gpui::Pixels> {
        let bounds = self.context.debug_bounds("live-app-disk-plot").unwrap();
        self.view.read_with(&self.context, |dashboard, _| {
            let viewport = dashboard.list.viewport_bounds();
            let preview = dashboard.preview.as_ref().map(|preview| {
                (
                    preview.source.clone(),
                    preview.target.second,
                    preview.complete,
                )
            });
            eprintln!(
                "QA_DISK_PLOT phase={phase} bounds={bounds:?} \
                 viewport={viewport:?} preview={preview:?}"
            );
            assert!(viewport.contains(&crate::charts::plot_bounds(bounds).center()));
        });
        bounds
    }

    fn seed_disk_readout(&mut self, readout: DiskReadout) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = disk_readout_snapshot(dashboard.generation, AppEpoch::Accepted, readout);
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn complete_disk_readout_history(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
            let preview = dashboard.preview.as_ref().unwrap();
            let generation = preview.generation.unwrap();
            let snapshot =
                disk_readout_snapshot(generation, AppEpoch::Historical, DiskReadout::Complete);
            assert_eq!(snapshot.selection["second"], preview.target.second);
            assert_eq!(
                Some(&snapshot.selection["selected"]),
                preview.target.identity.as_ref()
            );
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn assert_disk_readout(&mut self, expected: &DiskReadoutExpectation) {
        self.view.read_with(&self.context, |dashboard, _| {
            let panel = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-app-disk")
                .unwrap();
            let (second, reading) = dashboard.chart_accessibility_reading(panel);
            assert_eq!(second, Some(expected.second));
            assert!(reading.contains(&format!("Unix second {}", expected.second)));
            assert!(reading.ends_with(expected.headline), "{reading}");
        });
        assert!(self.text_is_visible(expected.headline_selector));
        assert!(self.text_is_visible(expected.legend));
    }

    fn assert_complete_disk_readout(&mut self) {
        self.assert_disk_readout(&DiskReadoutExpectation {
            second: 1000,
            headline: "3 MB/s",
            headline_selector: "chart-reading:live-app-disk:3 MB/s",
            legend: "chart-legend:live-app-disk:Example App:3 MB/s",
        });
    }

    fn assert_partial_disk_readout(&mut self) {
        self.assert_disk_readout(&DiskReadoutExpectation {
            second: 999,
            headline: "2.5 MB/s · known subtotal · 1 measured · 2 unavailable",
            headline_selector: "chart-reading:live-app-disk:2.5 MB/s *",
            legend: concat!(
                "chart-legend:live-app-disk:Example App:",
                "2.5 MB/s · known subtotal · 1 measured · 2 unavailable",
            ),
        });
    }

    fn seed_disk_interval(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = disk_snapshot(dashboard.generation);
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn selected_disk_executable(&self) -> Value {
        self.view.read_with(&self.context, |dashboard, _| {
            assert_eq!(dashboard.reading_executables().len(), 1);
            dashboard.reading_executables()[0].json()
        })
    }

    fn complete_disk_history(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
            let preview = dashboard.preview.as_ref().unwrap();
            let generation = preview.generation.unwrap();
            let snapshot = historical_disk_snapshot(generation);
            assert_eq!(snapshot.generation, generation);
            assert_eq!(snapshot.selection["second"], preview.target.second);
            assert_eq!(
                Some(&snapshot.selection["selected"]),
                preview.target.identity.as_ref()
            );
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn fail_disk_history(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
            let generation = dashboard.preview.as_ref().unwrap().generation.unwrap();
            assert!(dashboard.preview_receive(Update::Failed {
                generation,
                message: "Disk details failed".into(),
            }));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn hover_disk_gap(&mut self) {
        let plot = crate::charts::plot_bounds(self.chart_bounds());
        let position = gpui::point(
            plot.left() + plot.size.width * (897.5 / 900.0),
            plot.center().y,
        );
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
        });
        self.context.run_until_parked();
    }

    fn assert_current_disk_visible(&mut self) {
        assert!(self.text_is_visible("summary-card:Disk I/O:1.5 MB/s *"));
        assert!(self.text_is_visible("app-disk:bundle:41:1.5 MB/s *"));
        assert!(self.text_is_visible("executable-disk:hex:61:1.5 MB/s *"));
    }

    fn assert_current_disk_hidden(&mut self) {
        assert!(!self.text_is_visible("summary-card:Disk I/O:1.5 MB/s *"));
        assert!(!self.text_is_visible("app-disk:bundle:41:1.5 MB/s *"));
        assert!(!self.text_is_visible("executable-disk:hex:61:1.5 MB/s *"));
    }

    fn assert_historical_disk_visible(&mut self) {
        assert!(self.text_is_visible("summary-card:Disk I/O:3 MB/s"));
        assert!(self.text_is_visible("app-disk:bundle:41:3 MB/s"));
        assert!(self.text_is_visible("executable-disk:hex:63:3.0 MB/s"));
        assert_eq!(
            self.selected_disk_executable()["disk_io_bytes"],
            complete_measure("6000000")
        );
    }

    fn assert_historical_disk_hidden(&mut self) {
        assert!(!self.text_is_visible("summary-card:Disk I/O:3 MB/s"));
        assert!(!self.text_is_visible("app-disk:bundle:41:3 MB/s"));
        assert!(!self.text_is_visible("executable-disk:hex:63:3.0 MB/s"));
    }

    fn assert_disk_history(&self, second: u64, headline: &str, index: usize, rate: f32) {
        self.view.read_with(&self.context, |dashboard, _| {
            let panel = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-app-disk")
                .unwrap();
            let (observed, reading) = dashboard.chart_accessibility_reading(panel);
            assert_eq!(observed, Some(second));
            assert!(reading.contains(&format!("Unix second {second}")));
            assert!(reading.ends_with(headline), "{reading}");
            assert_eq!(panel.lines.len(), 1);
            assert_eq!(panel.lines[0].values[index], Some(rate));
        });
    }

    fn assert_disk_gap(&self) {
        self.view.read_with(&self.context, |dashboard, _| {
            let reading = dashboard.reading_snapshot().unwrap();
            assert_eq!(reading.selection["second"], 998);
            assert!(reading.selection["selected"].is_null());
            assert!(reading.detail.is_none());
            let panel = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-app-disk")
                .unwrap();
            let (second, headline) = dashboard.chart_accessibility_reading(panel);
            assert_eq!(second, Some(998));
            assert!(headline.ends_with("Unavailable"), "{headline}");
        });
    }

    fn assert_disk_selection_unchanged(&self) {
        self.view.read_with(&self.context, |dashboard, _| {
            assert_eq!(dashboard.request.second, Some(1000));
            assert_eq!(dashboard.request.identity, Some(super::identity(2)));
            assert_eq!(
                dashboard.snapshot.as_ref().unwrap().selection["selected"],
                super::identity(2)
            );
            assert!(dashboard.preview.is_none());
        });
    }
}

#[derive(Clone, Copy)]
struct DiskAmounts {
    read: u64,
    written: u64,
}

#[derive(Clone, Copy)]
enum DiskReadout {
    Small,
    Zero,
    Unavailable,
    Complete,
    Partial,
    WritePartial,
    ReadPartial,
    ReadUnavailable,
    WritePartialHistory,
}

fn disk_readout_snapshot(generation: u64, epoch: AppEpoch, current: DiskReadout) -> Box<Snapshot> {
    let historical = match current {
        DiskReadout::Complete => DiskReadout::Partial,
        DiskReadout::WritePartialHistory => DiskReadout::WritePartial,
        other => other,
    };
    let readout = match epoch {
        AppEpoch::Accepted => current,
        AppEpoch::Historical => historical,
        AppEpoch::LargerHistorical => panic!("Disk readouts use exactly three members"),
    };
    let mut snapshot = epoch.snapshot(generation);
    let app = disk_readout_app(readout, epoch);
    snapshot.apps.as_mut().unwrap()["apps"][0] = app.clone();
    let detail = snapshot.detail.as_mut().unwrap();
    detail["app"] = app;
    detail["elapsed_ns"] = json!("2000000000");
    detail["page"]["total"] = json!(3);
    detail["processes"] = disk_readout_processes(readout);
    snapshot.history.as_mut().unwrap()["points"] = json!([
        disk_readout_point(historical, AppEpoch::Historical),
        disk_readout_point(current, AppEpoch::Accepted)
    ]);

    snapshot
}

fn disk_readout_app(readout: DiskReadout, epoch: AppEpoch) -> Value {
    let mut app = epoch.app();
    app["members"] = json!(3);
    app["live_members"] = json!(3);
    let (read, written, joint) = match readout {
        DiskReadout::Small
        | DiskReadout::WritePartial
        | DiskReadout::ReadPartial
        | DiskReadout::WritePartialHistory => (4096, 1024, 5120),
        DiskReadout::ReadUnavailable => (0, 1024, 0),
        DiskReadout::Zero | DiskReadout::Unavailable => (0, 0, 0),
        DiskReadout::Complete => (4_000_000, 2_000_000, 6_000_000),
        DiskReadout::Partial => (4_000_000, 2_000_000, 5_000_000),
    };
    let measured = match readout {
        DiskReadout::Unavailable => [0, 0, 0],
        DiskReadout::Partial => [2, 1, 1],
        DiskReadout::WritePartial => [3, 1, 1],
        DiskReadout::ReadPartial => [1, 3, 1],
        DiskReadout::ReadUnavailable => [0, 3, 0],
        DiskReadout::Small
        | DiskReadout::Zero
        | DiskReadout::Complete
        | DiskReadout::WritePartialHistory => [3, 3, 3],
    };
    disk_readout_metric(&mut app, "disk_read", read, measured[0]);
    disk_readout_metric(&mut app, "disk_written", written, measured[1]);
    disk_readout_metric(&mut app, "disk_io", joint, measured[2]);

    app
}

fn disk_readout_metric(app: &mut Value, metric: &str, known: u64, measured: u64) {
    assert!(measured <= 3);
    app[format!("{metric}_bytes")] = json!({
        "known":known.to_string(),"measured_members":measured,
        "unavailable_members":3 - measured,"overflowed":false
    });
    app[format!("{metric}_bps")] = if measured == 0 {
        Value::Null
    } else {
        json!(known.to_f64().unwrap() / 2.0)
    };
}

fn disk_readout_point(readout: DiskReadout, epoch: AppEpoch) -> Value {
    let mut point = disk_readout_app(readout, epoch);
    point["observation"] = match epoch {
        AppEpoch::Accepted => wire_observation(1000, 2),
        AppEpoch::Historical => wire_observation(999, 1),
        AppEpoch::LargerHistorical => panic!("Disk history has two exact observations"),
    };
    point["segment"] = json!(0);
    point["source_available"] = json!(true);
    point["elapsed_ns"] = json!("2000000000");

    point
}

fn disk_readout_processes(readout: DiskReadout) -> Value {
    let amounts = disk_readout_process_amounts(readout);
    let mut processes = json!([
        disk_process(10, amounts[0]),
        disk_process(11, amounts[1]),
        disk_process(12, amounts[2])
    ]);
    match readout {
        DiskReadout::Unavailable => {
            for process in processes.as_array_mut().unwrap().iter_mut().take(3) {
                disk_readout_process_unavailable(process, "disk_read");
                disk_readout_process_unavailable(process, "disk_written");
                disk_readout_process_unavailable(process, "disk_io");
            }
        }
        DiskReadout::Partial => {
            disk_readout_process_unavailable(&mut processes[1], "disk_written");
            disk_readout_process_unavailable(&mut processes[1], "disk_io");
            disk_readout_process_unavailable(&mut processes[2], "disk_read");
            disk_readout_process_unavailable(&mut processes[2], "disk_written");
            disk_readout_process_unavailable(&mut processes[2], "disk_io");
        }
        DiskReadout::WritePartial => {
            for process in processes.as_array_mut().unwrap().iter_mut().skip(1).take(2) {
                disk_readout_process_unavailable(process, "disk_written");
                disk_readout_process_unavailable(process, "disk_io");
            }
        }
        DiskReadout::ReadPartial => {
            for process in processes.as_array_mut().unwrap().iter_mut().skip(1).take(2) {
                disk_readout_process_unavailable(process, "disk_read");
                disk_readout_process_unavailable(process, "disk_io");
            }
        }
        DiskReadout::ReadUnavailable => {
            for process in processes.as_array_mut().unwrap().iter_mut().take(3) {
                disk_readout_process_unavailable(process, "disk_read");
                disk_readout_process_unavailable(process, "disk_io");
            }
        }
        DiskReadout::Small
        | DiskReadout::Zero
        | DiskReadout::Complete
        | DiskReadout::WritePartialHistory => {}
    }

    processes
}

fn disk_readout_process_amounts(readout: DiskReadout) -> [DiskAmounts; 3] {
    match readout {
        DiskReadout::Small
        | DiskReadout::WritePartial
        | DiskReadout::ReadPartial
        | DiskReadout::WritePartialHistory => [
            DiskAmounts {
                read: 4096,
                written: 1024,
            },
            DiskAmounts {
                read: 0,
                written: 0,
            },
            DiskAmounts {
                read: 0,
                written: 0,
            },
        ],
        DiskReadout::Zero | DiskReadout::Unavailable => {
            [DiskAmounts {
                read: 0,
                written: 0,
            }; 3]
        }
        DiskReadout::ReadUnavailable => [
            DiskAmounts {
                read: 0,
                written: 1024,
            },
            DiskAmounts {
                read: 0,
                written: 0,
            },
            DiskAmounts {
                read: 0,
                written: 0,
            },
        ],
        DiskReadout::Complete | DiskReadout::Partial => [
            DiskAmounts {
                read: 3_000_000,
                written: 2_000_000,
            },
            DiskAmounts {
                read: 1_000_000,
                written: 0,
            },
            DiskAmounts {
                read: 0,
                written: 0,
            },
        ],
    }
}

fn disk_readout_process_unavailable(process: &mut Value, metric: &str) {
    process[format!("{metric}_bytes")] = json!({"known":null,"unavailable":"counter_reset"});
    process[format!("{metric}_bps")] = Value::Null;
}

fn disk_snapshot(generation: u64) -> Box<Snapshot> {
    let mut snapshot = AppEpoch::Accepted.snapshot(generation);
    let app = current_disk_app();
    snapshot.apps.as_mut().unwrap()["apps"][0] = app.clone();
    let mut crossing = disk_process(
        12,
        DiskAmounts {
            read: 250_000,
            written: 250_000,
        },
    );
    crossing["app_attribution"] = json!("exec");
    let detail = snapshot.detail.as_mut().unwrap();
    detail["app"] = app.clone();
    detail["elapsed_ns"] = json!("2000000000");
    detail["processes"] = json!([
        disk_process(
            10,
            DiskAmounts {
                read: 1_500_000,
                written: 500_000
            }
        ),
        disk_process(
            11,
            DiskAmounts {
                read: 500_000,
                written: 500_000
            }
        ),
        crossing
    ]);
    snapshot.history.as_mut().unwrap()["points"] = disk_history_points();

    snapshot
}

fn historical_disk_snapshot(generation: u64) -> Box<Snapshot> {
    let mut snapshot = AppEpoch::Historical.snapshot(generation);
    let app = historical_disk_app();
    snapshot.apps.as_mut().unwrap()["apps"][0] = app.clone();
    let detail = snapshot.detail.as_mut().unwrap();
    detail["app"] = app;
    detail["elapsed_ns"] = json!("2000000000");
    detail["processes"] = json!([
        disk_process(
            20,
            DiskAmounts {
                read: 3_000_000,
                written: 1_000_000
            }
        ),
        disk_process(
            21,
            DiskAmounts {
                read: 1_000_000,
                written: 1_000_000
            }
        )
    ]);
    for process in detail["processes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .take(2)
    {
        process["executable_name"] = json!({"display":"Historical worker","bytes_hex":"63"});
    }
    snapshot.history.as_mut().unwrap()["points"] = disk_history_points();

    snapshot
}

fn current_disk_app() -> Value {
    let mut app = AppEpoch::Accepted.app();
    app["disk_read_bytes"] = disk_measure("2000000");
    app["disk_written_bytes"] = disk_measure("1000000");
    app["disk_io_bytes"] = disk_measure("3000000");
    app["disk_read_bps"] = json!(1_000_000.0);
    app["disk_written_bps"] = json!(500_000.0);
    app["disk_io_bps"] = json!(1_500_000.0);

    app
}

fn historical_disk_app() -> Value {
    let mut app = AppEpoch::Historical.app();
    app["disk_read_bytes"] = complete_measure("4000000");
    app["disk_written_bytes"] = complete_measure("2000000");
    app["disk_io_bytes"] = complete_measure("6000000");
    app["disk_read_bps"] = json!(2_000_000.0);
    app["disk_written_bps"] = json!(1_000_000.0);
    app["disk_io_bps"] = json!(3_000_000.0);

    app
}

fn disk_history_points() -> Value {
    let mut current = current_disk_app();
    current["observation"] = wire_observation(1000, 2);
    current["segment"] = json!(0);
    current["source_available"] = json!(true);
    current["elapsed_ns"] = json!("2000000000");
    let mut historical = historical_disk_app();
    historical["observation"] = wire_observation(999, 1);
    historical["segment"] = json!(0);
    historical["source_available"] = json!(true);
    historical["elapsed_ns"] = json!("2000000000");

    json!([historical, current])
}

fn disk_process(pid: u32, amounts: DiskAmounts) -> Value {
    let mut process = super::process("Accepted worker", "61", pid);
    let combined = amounts.read.checked_add(amounts.written).unwrap();
    process["disk_read_bytes"] = json!({"known":amounts.read.to_string(),"unavailable":null});
    process["disk_written_bytes"] = json!({"known":amounts.written.to_string(),"unavailable":null});
    process["disk_io_bytes"] = json!({"known":combined.to_string(),"unavailable":null});
    process["disk_read_bps"] = json!(amounts.read.to_f64().unwrap() / 2.0);
    process["disk_written_bps"] = json!(amounts.written.to_f64().unwrap() / 2.0);
    process["disk_io_bps"] = json!(combined.to_f64().unwrap() / 2.0);

    process
}

fn disk_measure(known: &str) -> Value {
    json!({"known":known,"measured_members":2,"unavailable_members":1,"overflowed":false})
}

fn complete_measure(known: &str) -> Value {
    json!({"known":known,"measured_members":2,"unavailable_members":0,"overflowed":false})
}
