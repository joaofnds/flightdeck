use gpui::{TestAppContext, px};

use super::{InputDriver, InputHarness};

#[gpui::test]
fn computer_model_card_shows_the_prototype_specifications_above_the_reference(
    cx: &mut TestAppContext,
) {
    // Goal: the model card reads as the prototype's, with no system details below it.
    // Method: seed this Mac's inventory and read each rendered specification and section.
    let mut harness = InputHarness::setup_route(cx, "computer");

    harness.driver.seed_computer_inventory("Mac17,8");

    for reading in [
        "computer-model:MacBook Pro",
        "computer-generation:16-inch · 2026",
        "computer-spec:Model identifier:Mac17,8",
        "computer-spec:Chip:Apple M5 Pro",
        "computer-spec:CPU:18 cores",
        "computer-spec:GPU:20 cores",
        "computer-spec:Memory:48 GiB unified memory",
        "computer-schematic-reference",
    ] {
        assert!(harness.driver.reading_is_visible(reading), "{reading}");
    }
    assert!(!harness.driver.reading_is_visible("computer-system-details"));
    harness.driver.assert_computer_sections_ordered();
    harness.driver.assert_computer_model_reference_layout();
    harness.teardown();
}

#[gpui::test]
fn computer_reference_shows_the_prototype_assembly_and_airflow_legend(cx: &mut TestAppContext) {
    // Goal: the open schematic reference names its reference assembly and the airflow scale.
    // Method: seed this Mac's inventory and read the reference's heading and legend stops.
    let mut harness = InputHarness::setup_route(cx, "computer");

    harness.driver.seed_computer_inventory("Mac17,8");

    for reading in [
        "computer-reference-assembly:MacBook Pro 16″ · 2026 · Apple M5 Pro · Mac17,8 · A3428",
        "computer-airflow-stop:50 °C",
        "computer-airflow-stop:70 °C",
        "computer-airflow-stop:90 °C",
    ] {
        assert!(harness.driver.reading_is_visible(reading), "{reading}");
    }
    harness.teardown();
}

impl InputDriver {
    fn seed_computer_inventory(&mut self, model: &str) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.inventory = vec![
                ("Model identifier".into(), model.into()),
                ("Chip".into(), "Apple M5 Pro".into()),
                ("Architecture".into(), "arm64".into()),
                ("CPU cores".into(), "18".into()),
                ("GPU cores".into(), "20".into()),
                ("Memory".into(), "48 GiB".into()),
            ];
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1200.0), px(2000.0)));
        self.context.run_until_parked();
    }

    fn assert_computer_sections_ordered(&mut self) {
        let model = self
            .context
            .debug_bounds("computer-specifications")
            .unwrap();
        let reference = self
            .context
            .debug_bounds("computer-schematic-reference")
            .unwrap();
        assert!(model.bottom() <= reference.top());
    }
}

#[gpui::test]
fn computer_specifications_fit_a_narrow_view(cx: &mut TestAppContext) {
    // Goal: a narrow window keeps every specification inside the model card.
    // Method: resize below the card's width and compare each reading with the card's bounds.
    let mut harness = InputHarness::setup_route(cx, "computer");
    harness.driver.seed_computer_inventory("Mac17,8");

    harness
        .driver
        .context
        .simulate_resize(gpui::size(px(620.0), px(2400.0)));
    harness.driver.context.run_until_parked();

    harness.driver.assert_computer_sections_ordered();
    harness
        .driver
        .assert_computer_specification_fits("computer-spec:Chip:Apple M5 Pro");
    harness
        .driver
        .assert_computer_specification_fits("computer-spec:Memory:48 GiB unified memory");
    harness.teardown();
}

#[gpui::test]
fn a_different_computer_model_keeps_its_reported_identity_without_a_guessed_generation(
    cx: &mut TestAppContext,
) {
    // Goal: another Mac shows what it reports and no title or generation guessed for it.
    // Method: seed another identifier, chip, core count and memory size and read the card.
    let mut harness = InputHarness::setup_route(cx, "computer");

    harness.driver.seed_distinct_computer_inventory();

    for reading in [
        "computer-model:This Mac",
        "computer-spec:Model identifier:Mac99,1",
        "computer-spec:Chip:Apple M2 Max",
        "computer-spec:CPU:12 cores",
        "computer-spec:Memory:96 GiB unified memory",
    ] {
        assert!(harness.driver.reading_is_visible(reading), "{reading}");
    }
    assert!(
        !harness
            .driver
            .reading_is_visible("computer-generation:16-inch · 2026")
    );
    harness.teardown();
}

impl InputDriver {
    fn assert_computer_model_reference_layout(&mut self) {
        let panel = self
            .context
            .debug_bounds("computer-specifications")
            .unwrap();
        assert!(panel.size.width <= px(760.0));
        let heading = self
            .context
            .debug_bounds("computer-model:MacBook Pro")
            .unwrap();
        assert!(heading.size.height <= px(24.0));
        let label = self
            .context
            .debug_bounds("computer-spec-label:Chip")
            .unwrap();
        let reading = self
            .context
            .debug_bounds("computer-spec:Chip:Apple M5 Pro")
            .unwrap();
        assert_eq!(label.top(), reading.top());
        assert!(label.right() < reading.left());
    }

    fn seed_distinct_computer_inventory(&mut self) {
        self.seed_computer_inventory("Mac99,1");
        self.view.update(&mut self.context, |dashboard, cx| {
            for (label, reading) in [
                ("Chip", "Apple M2 Max"),
                ("CPU cores", "12"),
                ("Memory", "96 GiB"),
            ] {
                dashboard
                    .inventory
                    .iter_mut()
                    .find(|(name, _)| name == label)
                    .unwrap()
                    .1 = reading.into();
            }
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn assert_computer_specification_fits(&mut self, selector: &'static str) {
        let panel = self
            .context
            .debug_bounds("computer-specifications")
            .unwrap();
        let reading = self.context.debug_bounds(selector).expect(selector);
        assert!(panel.left() <= reading.left());
        assert!(reading.right() <= panel.right());
    }
}

#[gpui::test]
fn computer_reference_inline_link_opens_its_source_when_clicked(cx: &mut TestAppContext) {
    // Goal: a linked run in the reference text opens its source, as the prototype's links do.
    // Method: click the first characters of the paragraph that opens with Apple's service view.
    let mut harness = InputHarness::setup_route(cx, "computer");
    harness.driver.seed_computer_inventory("Mac17,8");

    harness
        .driver
        .click_computer_reference_start("computer-reference-paragraph:1");

    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://support.apple.com/en-us/125827")
    );
    harness.teardown();
}

#[gpui::test]
fn computer_reference_plain_text_opens_nothing_when_clicked(cx: &mut TestAppContext) {
    // Goal: only linked runs open a source. Method: click the start of the first paragraph,
    // which links nothing.
    let mut harness = InputHarness::setup_route(cx, "computer");
    harness.driver.seed_computer_inventory("Mac17,8");

    harness
        .driver
        .click_computer_reference_start("computer-reference-paragraph:0");

    assert!(cx.opened_url().is_none());
    harness.teardown();
}

impl InputDriver {
    fn click_computer_reference_start(&mut self, paragraph: &'static str) {
        let bounds = self.context.debug_bounds(paragraph).unwrap();
        let position = bounds.origin + gpui::point(px(12.0), px(6.0));
        self.context.simulate_mouse_down(
            position,
            gpui::MouseButton::Left,
            gpui::Modifiers::none(),
        );
        self.context
            .simulate_mouse_up(position, gpui::MouseButton::Left, gpui::Modifiers::none());
        self.context.run_until_parked();
    }
}

#[gpui::test]
fn computer_header_names_its_snapshot_source_without_time_controls(cx: &mut TestAppContext) {
    // Goal: the Computer header reads as the prototype's, a source and no time selection,
    // since the page shows no recorded second. Method: open Computer and read the header.
    let mut harness = InputHarness::setup_route(cx, "computer");

    harness.driver.seed_computer_inventory("Mac17,8");

    assert!(
        harness
            .driver
            .reading_is_visible("source-status:System snapshot")
    );
    assert!(!harness.driver.reading_is_visible("time-range"));
    assert!(!harness.driver.reading_is_visible("reading-selected"));
    harness.teardown();
}

#[gpui::test]
fn computer_header_keeps_its_source_as_far_down_as_on_a_page_with_time_controls(
    cx: &mut TestAppContext,
) {
    // Goal: the source pill sits where it does beside the prototype's range picker, 44 px
    // tall. Method: open Computer and measure the header's controls.
    let mut harness = InputHarness::setup_route(cx, "computer");

    harness.driver.seed_computer_inventory("Mac17,8");

    let controls = harness.driver.context.debug_bounds("reading-controls");
    assert_eq!(controls.unwrap().size.height, px(44.0));
    harness.teardown();
}

#[gpui::test]
fn self_latest_report_button_is_as_tall_as_the_prototypes(cx: &mut TestAppContext) {
    // Goal: the button that returns to the latest report is 44 px tall, as the prototype's is.
    // Method: open Self-monitoring with a run started long ago and measure the button.
    let mut harness = InputHarness::setup_route(cx, "self");
    harness.driver.seed_self_run();

    let button = harness.driver.context.debug_bounds("self-latest-report");
    assert_eq!(button.unwrap().size.height, px(44.0));
    harness.teardown();
}

#[gpui::test]
fn self_plot_sits_where_the_prototypes_does_in_its_card(cx: &mut TestAppContext) {
    // Goal: the plot is 180 px tall, starts 54 px of value labels in and ends 12 px short of the
    // card's padding, as the prototype's does. Method: open Self-monitoring with a run started
    // long ago and compare the plot's bounds with its card's.
    let mut harness = InputHarness::setup_route(cx, "self");
    harness.driver.seed_self_run();

    let card = harness.driver.context.debug_bounds("chart-card:self-late");
    let plot = harness.driver.context.debug_bounds("self-plot:self-late");

    let (card, plot) = (card.unwrap(), plot.unwrap());
    assert_eq!(plot.size.height, px(180.0));
    assert_eq!(plot.left() - card.left(), px(13.0 + 54.0));
    assert_eq!(card.right() - plot.right(), px(13.0 + 4.0));
    harness.teardown();
}

#[gpui::test]
fn self_budget_label_ends_at_the_plot_in_the_prototypes_size(cx: &mut TestAppContext) {
    // Goal: the budget line's label is 12 px text ending at the plot's right edge, above the
    // line, as the prototype's is. Method: open Self-monitoring and compare the label's bounds
    // with the plot's.
    let mut harness = InputHarness::setup_route(cx, "self");
    harness.driver.seed_self_run();

    let plot = harness.driver.context.debug_bounds("self-plot:self-budget");
    let label = harness.driver.context.debug_bounds("self-budget-label");

    let (plot, label) = (plot.unwrap(), label.unwrap());
    assert_eq!(label.size.height, px(15.0));
    assert_eq!(label.right(), plot.right() - px(8.0));
    harness.teardown();
}

#[gpui::test]
fn self_header_keeps_its_source_without_time_controls(cx: &mut TestAppContext) {
    // Goal: Self-monitoring shows no time selection, as in the prototype, since its readings
    // do not follow chart inspection. Method: open Self-monitoring and read the header.
    let mut harness = InputHarness::setup_route(cx, "self");

    harness
        .driver
        .context
        .simulate_resize(gpui::size(px(1200.0), px(2000.0)));
    harness.driver.context.run_until_parked();

    assert!(harness.driver.reading_is_visible("reading-controls"));
    assert!(!harness.driver.reading_is_visible("time-range"));
    assert!(!harness.driver.reading_is_visible("reading-selected"));
    harness.teardown();
}

impl InputDriver {
    fn seed_self_run(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut reports = crate::self_reports::SelfReports::default();
            reports.begin(1);
            dashboard.self_reports = std::rc::Rc::new(reports);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1200.0), px(2000.0)));
        self.context.run_until_parked();
    }
}
