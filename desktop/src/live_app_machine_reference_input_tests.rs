use super::*;

#[gpui::test]
fn machine_cpu_reference_follows_historical_hover_and_restores_on_exit(cx: &mut TestAppContext) {
    // Installed pointer input must change both the real reference and the App subtotal.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_machine_comparisons();
    assert!(harness.driver.reading_is_visible(
        "chart-legend:live-cpu-apps:All CPU:450.00 % · all recorded logical CPUs"
    ));

    harness
        .driver
        .hover_previous_second_on("live-cpu-apps-plot");

    assert_historical_cpu_reference(&mut harness.driver);
    assert!(harness.driver.reading_is_visible(
        "chart-tooltip-row:live-cpu-apps:2:All CPU:150.00 % · all recorded logical CPUs"
    ));
    let (second, reading) = harness.driver.chart_accessibility_reading("live-cpu-apps");
    assert_eq!(second, Some(999));
    assert!(reading.ends_with("130.00 % captured; All CPU: 150.00 % · all recorded logical CPUs"));
    harness.driver.complete_overview_history_preview();
    assert_historical_cpu_reference(&mut harness.driver);
    harness
        .driver
        .leave_plot("live-cpu-apps-plot", PlotExit::Sidebar);
    assert!(harness.driver.reading_is_visible(
        "chart-legend:live-cpu-apps:All CPU:450.00 % · all recorded logical CPUs"
    ));
    assert!(
        harness
            .driver
            .reading_is_visible("chart-reading:live-cpu-apps:250.00 % captured")
    );
    harness.teardown();
}

#[gpui::test]
fn machine_disk_reference_follows_historical_hover_without_changing_exact_app_bytes(
    cx: &mut TestAppContext,
) {
    // Whole-device MB/s and captured App bytes remain independent through real graph input.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_machine_comparisons();
    assert!(harness.driver.reading_is_visible(
        "chart-legend:live-disk-apps:All disk I/O:6.0 MB/s · whole-device read + write"
    ));

    harness
        .driver
        .hover_previous_second_on("live-disk-apps-plot");

    assert!(harness.driver.reading_is_visible(
        "chart-legend:live-disk-apps:All disk I/O:3.0 MB/s · whole-device read + write"
    ));
    assert!(harness.driver.reading_is_visible(
        "chart-tooltip-row:live-disk-apps:2:All disk I/O:3.0 MB/s · whole-device read + write"
    ));
    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-disk-apps:Current:500.0 kB/s · 1000000 B")
    );
    let (second, reading) = harness.driver.chart_accessibility_reading("live-disk-apps");
    assert_eq!(second, Some(999));
    assert!(
        reading
            .ends_with("500.0 kB/s captured; All disk I/O: 3.0 MB/s · whole-device read + write")
    );
    harness
        .driver
        .leave_plot("live-disk-apps-plot", PlotExit::Sidebar);
    assert!(harness.driver.reading_is_visible(
        "chart-legend:live-disk-apps:All disk I/O:6.0 MB/s · whole-device read + write"
    ));
    assert!(
        harness
            .driver
            .reading_is_visible("chart-legend:live-disk-apps:Current:1.5 MB/s · 3000000 B")
    );
    harness.teardown();
}

#[gpui::test]
fn machine_references_withhold_neighboring_readings_at_a_history_gap(cx: &mut TestAppContext) {
    // Missing wall seconds get no adjacent scalar reference or App subtotal.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_machine_comparisons();
    let gap = harness
        .driver
        .second_position("live-disk-apps-plot", 897.5 / 900.0);

    harness.driver.hover_position(gap);

    assert!(harness.driver.reading_is_visible(concat!(
        "chart-legend:live-disk-apps:All disk I/O:Unavailable",
        " · machine observation unavailable"
    )));
    assert!(harness.driver.reading_is_visible(concat!(
        "chart-legend:live-cpu-apps:All CPU:Unavailable",
        " · machine observation unavailable"
    )));
    let (second, reading) = harness.driver.chart_accessibility_reading("live-cpu-apps");
    assert_eq!(second, Some(998));
    assert!(
        reading.ends_with("Unavailable; All CPU: Unavailable · machine observation unavailable")
    );
    harness
        .driver
        .leave_plot("live-disk-apps-plot", PlotExit::Sidebar);
    assert!(harness.driver.reading_is_visible(
        "chart-legend:live-disk-apps:All disk I/O:6.0 MB/s · whole-device read + write"
    ));
    harness.teardown();
}

#[gpui::test]
fn machine_reference_data_reports_the_selected_reference_value(cx: &mut TestAppContext) {
    // Activate the installed Data control, whose Selected cell uses the shared reference readout.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_machine_comparisons();

    harness
        .driver
        .activate_machine_data("chart-data:live-disk-apps");

    assert!(harness.driver.reading_is_visible("All disk I/O"));
    assert!(
        harness
            .driver
            .reading_is_visible("6.0 MB/s · whole-device read + write")
    );
    assert!(harness.driver.reading_is_visible("1.5 MB/s · 3000000 B"));
    harness.teardown();
}

fn assert_historical_cpu_reference(driver: &mut InputDriver) {
    assert!(driver.reading_is_visible(
        "chart-legend:live-cpu-apps:All CPU:150.00 % · all recorded logical CPUs"
    ));
    assert!(driver.reading_is_visible("chart-reading:live-cpu-apps:130.00 % captured"));
}

fn comparison_snapshot(generation: u64) -> Box<Snapshot> {
    let mut snapshot = overview_history_snapshot(generation);
    snapshot.selection["trends"][0]["cpu_percent_one_core"] = json!(150.0);
    snapshot.selection["trends"][1]["cpu_percent_one_core"] = json!(450.0);
    snapshot.selection["trends"][0]["storage"] = json!({"state":"complete",
        "read_bps":2_000_000.0,"write_bps":1_000_000.0});
    snapshot.selection["trends"][1]["storage"] = json!({"state":"complete",
        "read_bps":4_000_000.0,"write_bps":2_000_000.0});
    snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
    let histories = Arc::get_mut(snapshot.overview_app_histories.as_mut().unwrap()).unwrap();
    histories.cpu = Arc::new(AppAttributionHistory::prepare(
        Metric::Cpu,
        &fixture(Metric::Cpu),
        &snapshot.selection,
    ));
    histories.disk = Arc::new(AppAttributionHistory::prepare(
        Metric::Disk,
        &crate::live_app_attribution_history::tests::disk_fixture(),
        &snapshot.selection,
    ));

    snapshot
}

impl InputDriver {
    fn seed_machine_comparisons(&mut self) {
        self.install_machine_comparisons(comparison_snapshot);
    }

    fn install_machine_comparisons(&mut self, snapshot: fn(u64) -> Box<Snapshot>) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.accept(snapshot(dashboard.generation));
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1100.0), px(6500.0)));
        self.context.run_until_parked();
    }

    fn activate_machine_data(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).unwrap();
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }
}

#[gpui::test]
fn machine_reference_preserves_app_bytes_above_float_integer_precision(cx: &mut TestAppContext) {
    // Read installed legends with an App counter that cannot round-trip through f32 or f64.
    let mut harness = InputHarness::setup_route(cx, "overview");

    harness
        .driver
        .install_machine_comparisons(large_disk_snapshot);

    assert!(harness.driver.reading_is_visible(
        "chart-legend:live-disk-apps:All disk I/O:6.0 MB/s · whole-device read + write"
    ));
    assert!(harness.driver.reading_is_visible(
        "chart-legend:live-disk-apps:Current:4503599616.0 MB/s · 9007199254740993 B"
    ));
    harness.teardown();
}

fn large_disk_snapshot(generation: u64) -> Box<Snapshot> {
    let mut snapshot = comparison_snapshot(generation);
    let mut apps = crate::live_app_attribution_history::tests::disk_fixture();
    apps["points"][1]["apps"][0]["disk_io_bytes"]["known"] = json!("9007199254740993");
    apps["points"][1]["apps"][0]["disk_io_bps"] = json!(4_503_599_627_370_496.0);
    Arc::get_mut(snapshot.overview_app_histories.as_mut().unwrap())
        .unwrap()
        .disk = Arc::new(AppAttributionHistory::prepare(
        Metric::Disk,
        &apps,
        &snapshot.selection,
    ));

    snapshot
}

struct MachineVisibility {
    legend: &'static str,
    data: &'static str,
    reference: &'static str,
    app: &'static str,
    collision_reference: &'static str,
    collision_app: &'static str,
}

macro_rules! machine_visibility_case {
    ($name:ident, $legend:literal, $data:literal, $reference:literal, $app:literal,
        $collision_reference:expr, $collision_app:literal) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // A rolling cohort must not transfer a hidden machine preference to a real App.
            let mut harness = InputHarness::setup_route(cx, "overview");
            harness.driver.seed_machine_comparisons();
            let expected = MachineVisibility {
                legend: $legend,
                data: $data,
                reference: $reference,
                app: $app,
                collision_reference: $collision_reference,
                collision_app: $collision_app,
            };
            harness.driver.click_resident_legend(expected.legend);
            expect_machine_visibility(&mut harness.driver, &expected);

            harness
                .driver
                .install_machine_comparisons(collision_snapshot);
            assert!(
                harness
                    .driver
                    .reading_is_visible(expected.collision_reference)
            );
            assert!(harness.driver.reading_is_visible(expected.collision_app));
            expect_machine_visibility(&mut harness.driver, &expected);
            harness
                .driver
                .install_machine_comparisons(comparison_snapshot);
            expect_machine_visibility(&mut harness.driver, &expected);
            harness.teardown();
        }
    };
}

machine_visibility_case!(
    machine_cpu_visibility_survives_app_name_collision_and_removal,
    "chart-legend:live-cpu-apps:All CPU:450.00 % · all recorded logical CPUs",
    "chart-data:live-cpu-apps",
    "450.00 % · all recorded logical CPUs",
    "240.00 %",
    concat!(
        "chart-legend:live-cpu-apps:All CPU (machine reference 1):",
        "450.00 % · all recorded logical CPUs"
    ),
    "chart-legend:live-cpu-apps:All CPU:240.00 %"
);
machine_visibility_case!(
    machine_disk_visibility_survives_app_name_collision_and_removal,
    "chart-legend:live-disk-apps:All disk I/O:6.0 MB/s · whole-device read + write",
    "chart-data:live-disk-apps",
    "6.0 MB/s · whole-device read + write",
    "1.5 MB/s · 3000000 B",
    concat!(
        "chart-legend:live-disk-apps:All disk I/O (machine reference 1):",
        "6.0 MB/s · whole-device read + write"
    ),
    "chart-legend:live-disk-apps:All disk I/O:1.5 MB/s · 3000000 B"
);

fn expect_machine_visibility(driver: &mut InputDriver, expected: &MachineVisibility) {
    driver.click_resident_legend(expected.data);
    assert!(!driver.reading_is_visible(expected.reference));
    assert!(driver.reading_is_visible(expected.app));
    driver.click_resident_legend("chart-data-close");
}

fn collision_snapshot(generation: u64) -> Box<Snapshot> {
    let mut snapshot = comparison_snapshot(generation);
    let mut cpu = fixture(Metric::Cpu);
    cpu["cohort"][0]["key"]["display"] = json!("All CPU");
    let mut disk = crate::live_app_attribution_history::tests::disk_fixture();
    disk["cohort"][0]["key"]["display"] = json!("All disk I/O");
    disk["cohort"].as_array_mut().unwrap().push(json!({"key":{
        "kind":"bundle", "bytes_hex":"42", "display":"Second/O"}}));
    for point in disk["points"].as_array_mut().unwrap() {
        let app = point["apps"][0].clone();
        point["apps"].as_array_mut().unwrap().push(app);
    }
    let histories = Arc::get_mut(snapshot.overview_app_histories.as_mut().unwrap()).unwrap();
    histories.cpu = Arc::new(AppAttributionHistory::prepare(
        Metric::Cpu,
        &cpu,
        &snapshot.selection,
    ));
    histories.disk = Arc::new(AppAttributionHistory::prepare(
        Metric::Disk,
        &disk,
        &snapshot.selection,
    ));

    snapshot
}
