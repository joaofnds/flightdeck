use super::*;

#[gpui::test]
fn all_groups_pointer_hover_shares_exact_readings_and_restores_on_exit(cx: &mut TestAppContext) {
    // Dispatch real plot input, then observe every installed group's timestamp and exact bytes.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.resize_process_history();
    harness.driver.deliver_all_group_history();

    harness.driver.hover_all_groups(898.5 / 900.0);

    assert!(
        harness
            .driver
            .text_is_visible("chart-legend:live-process-groups-cpu:b:120%")
    );
    assert!(
        harness
            .driver
            .text_is_visible("chart-tooltip-row:live-process-groups-cpu:1:b:120%")
    );
    assert!(
        harness
            .driver
            .text_is_visible("chart-legend:live-process-groups-disk:b read:9,007,199,232 MB/s")
    );
    harness.driver.assert_all_groups_historical_quantities();
    harness.driver.assert_all_groups_second(999);
    harness.driver.leave_all_groups();
    assert!(
        harness
            .driver
            .text_is_visible("chart-legend:live-process-groups-cpu:b:240%")
    );
    harness.driver.assert_all_groups_current_quantities();
    assert!(
        !harness
            .driver
            .text_is_visible("chart-tooltip:live-process-groups-cpu")
    );
    harness.driver.assert_all_groups_second(1000);
    harness.teardown();
}

#[gpui::test]
fn all_groups_gap_never_borrows_the_current_reading(cx: &mut TestAppContext) {
    // Goal: a missing second reads as missing and names the gap under the charts.
    // Method: point at a missing second through the installed chart, then inspect all eight
    // panels and the footnote, and leave to read the current second again.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.resize_process_history();
    harness.driver.deliver_all_group_history();

    harness.driver.hover_all_groups(897.5 / 900.0);

    assert!(
        harness
            .driver
            .text_is_visible("chart-legend:live-process-groups-cpu:b:Unavailable")
    );
    assert!(
        harness
            .driver
            .text_is_visible("chart-legend:live-process-groups-disk:b read:Unavailable")
    );
    harness.driver.assert_all_groups_unavailable_quantities();
    harness.driver.assert_all_groups_second(998);
    harness.driver.leave_all_groups();
    harness.driver.assert_all_groups_current_quantities();
    assert!(
        !harness
            .driver
            .text_is_visible("No process group readings at this second.")
    );
    harness.driver.assert_all_groups_second(1000);
    harness.teardown();
}

#[gpui::test]
fn all_groups_installed_data_and_legend_controls_preserve_exact_selected_values(
    cx: &mut TestAppContext,
) {
    // Open the real Disk Data control, then toggle a real legend without hiding another metric.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.resize_process_history();
    harness.driver.deliver_all_group_history();

    harness
        .driver
        .process_history_control("chart-data:live-process-groups-disk");
    harness
        .driver
        .view
        .read_with(&harness.driver.context, |dashboard, _| {
            assert!(dashboard.chart_inspector.is_some());
            let disk = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-process-groups-disk")
                .unwrap();
            assert_eq!(disk.lines[0].values.iter().flatten().count(), 2);
        });

    assert!(harness.driver.text_is_visible("160 B/s"));
    assert!(harness.driver.text_is_visible("80 B/s"));
    assert!(harness.driver.text_is_visible("80 B/s"));
    assert!(harness.driver.text_is_visible("2 / 2"));
    harness.driver.process_history_control("chart-data-close");
    harness
        .driver
        .process_history_control("chart-legend:live-process-groups-cpu:b:240%");
    harness.driver.assert_all_groups_hidden_cpu();
    harness.driver.assert_all_groups_current_quantities();
    harness.teardown();
}

fn group_b(panel: &crate::data::Panel) -> &crate::data::Line {
    panel.lines.iter().find(|line| line.name == "b").unwrap()
}

impl AppHoverDriver {
    fn deliver_all_group_history(&mut self) {
        let mut history = crate::live_source::tests::process_history::all_history();
        // Reuse a public worker payload with distinct historical and current quantities.
        history.points.truncate(2);
        history.app_bytes = Box::from(b"A".as_slice());
        history.selected.wall_ns = Some(1_000_000_000_000);
        history.points[0].observation.wall_ns = Some(999_000_000_000);
        history.points[1].observation.wall_ns = history.selected.wall_ns;
        history.points[0].elapsed_ns = Some(1_000_000_000);
        history.points[1].elapsed_ns = Some(1_000_000_000);
        history.points[0].readings = history.points[1].readings;
        history.points[0].readings[0].measures[0].known = 1_200_000_000;
        history.points[0].readings[0].measures[2].known = 9_007_199_254_740_993;
        history.points[0].readings[0].measures[1].known = 1_000_000_000;
        history.points[1].readings[0].measures[1].known = 2_000_000_000;
        history.points[0].readings[0].measures[5].known = 3_221_225_472;
        history.points[1].readings[0].measures[5].known = 6_442_450_944;
        history.points[0].readings[0].measures[6].known = 5_368_709_120;
        history.points[1].readings[0].measures[6].known = 9_663_676_416;
        history.points[0].readings[0].measures[7].known = 5;
        history.points[1].readings[0].measures[7].known = 11;
        history.points[0].readings[0].measures[8].known = 3;
        history.points[1].readings[0].measures[8].known = 7;
        history.points[0].readings[0].measures[9].known = 8;
        history.points[1].readings[0].measures[9].known = 13;
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = AppEpoch::Accepted.snapshot(dashboard.generation);
            snapshot.process_history = Some(std::sync::Arc::new(ProcessHistory::prepare_all(
                history,
                &dashboard.request,
                &snapshot.selection,
            )));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
        self.assert_all_groups_plot_values();
    }

    fn assert_all_groups_plot_values(&self) {
        self.view.read_with(&self.context, |dashboard, _| {
            let wakeups = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-process-groups-wakeups")
                .unwrap();
            let pageins = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-process-groups-pageins")
                .unwrap();
            let threads = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-process-groups-threads")
                .unwrap();

            assert_eq!(group_b(wakeups).values, [Some(5.0), Some(11.0)]);
            assert_eq!(group_b(pageins).values, [Some(3.0), Some(7.0)]);
            assert_eq!(group_b(threads).values, [Some(8.0), Some(13.0)]);
        });
    }

    fn hover_all_groups(&mut self, fraction: f32) {
        let bounds = self
            .context
            .debug_bounds("live-process-groups-cpu-plot")
            .unwrap();
        let plot = crate::charts::plot_bounds(bounds);
        self.context.simulate_mouse_move(
            gpui::point(plot.left() + plot.size.width * fraction, plot.center().y),
            None,
            Modifiers::none(),
        );
        self.context.run_until_parked();
    }

    fn leave_all_groups(&mut self) {
        self.context.simulate_mouse_move(
            gpui::point(gpui::px(20.0), gpui::px(500.0)),
            None,
            Modifiers::none(),
        );
        self.context.run_until_parked();
    }

    fn assert_all_groups_historical_quantities(&mut self) {
        assert!(self.text_is_visible("chart-legend:live-process-groups-resident:b:3 GiB"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-footprint:b:5 GiB"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-power:b:1 W"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-wakeups:b:5 /s"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-pageins:b:3 pages/s"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-threads:b:8 threads"));
    }

    fn assert_all_groups_current_quantities(&mut self) {
        assert!(self.text_is_visible("chart-legend:live-process-groups-resident:b:6 GiB"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-footprint:b:9 GiB"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-power:b:2 W"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-wakeups:b:11 /s"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-pageins:b:7 pages/s"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-threads:b:13 threads"));
    }

    fn assert_all_groups_unavailable_quantities(&mut self) {
        assert!(self.text_is_visible("chart-legend:live-process-groups-resident:b:Unavailable"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-footprint:b:Unavailable"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-power:b:Unavailable"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-wakeups:b:Unavailable"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-pageins:b:Unavailable"));
        assert!(self.text_is_visible("chart-legend:live-process-groups-threads:b:Unavailable"));
    }

    fn assert_all_groups_second(&self, expected: u64) {
        self.view.read_with(&self.context, |dashboard, _| {
            let panels: Vec<_> = dashboard
                .panels
                .iter()
                .filter(|panel| panel.id.starts_with("live-process-groups-"))
                .collect();
            assert_eq!(panels.len(), 12);
            for panel in panels {
                assert_eq!(
                    dashboard.chart_accessibility_reading(panel).0,
                    Some(expected)
                );
            }
        });
    }

    fn assert_all_groups_hidden_cpu(&self) {
        self.view.read_with(&self.context, |dashboard, _| {
            assert!(
                dashboard
                    .hidden
                    .iter()
                    .any(|key| key == "live-process-groups-cpu\tb")
            );
            assert!(
                !dashboard
                    .hidden
                    .iter()
                    .any(|key| key == "live-process-groups-disk\tb read")
            );
        });
    }
}

#[gpui::test]
fn all_groups_stack_as_the_prototypes_areas(cx: &mut TestAppContext) {
    // Goal: process group histories stack as filled areas, and disk and network keep their two
    // directions apart as lines, as the prototype draws them.
    // Method: deliver every group's history and read the installed panels' stacking and fill.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.resize_process_history();
    harness.driver.deliver_all_group_history();

    harness
        .driver
        .view
        .read_with(&harness.driver.context, |dashboard, _| {
            let panels: Vec<_> = dashboard
                .panels
                .iter()
                .filter(|panel| panel.id.starts_with("live-process-groups-"))
                .collect();
            assert_eq!(panels.len(), 12);
            for panel in panels {
                if ["live-process-groups-disk", "live-process-groups-network"]
                    .contains(&panel.id.as_str())
                {
                    assert_eq!(panel.stack, Some(false));
                    assert_eq!(panel.fill, Some(0.1));
                } else {
                    assert_eq!(panel.stack, Some(true), "{}", panel.id);
                    assert_eq!(panel.fill, Some(0.45), "{}", panel.id);
                }
            }
        });
    harness.teardown();
}
