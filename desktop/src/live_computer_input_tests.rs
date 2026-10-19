use gpui::{TestAppContext, px};

use super::{InputDriver, InputHarness};

#[gpui::test]
fn computer_model_and_specifications_lead_the_independent_system_details(cx: &mut TestAppContext) {
    // Render the approved model from current inventory before the operating-system details.
    let mut harness = InputHarness::setup_route(cx, "computer");
    harness.driver.seed_computer_inventory("Mac17,8");

    assert!(
        harness
            .driver
            .reading_is_visible("computer-model:MacBook Pro")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("computer-generation:16-inch · 2026")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("computer-spec:Chip:Apple M5 Pro")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("computer-spec:Physical CPU cores:18")
    );
    assert!(harness.driver.reading_is_visible("computer-system-details"));
    assert!(
        harness
            .driver
            .reading_is_visible("computer-schematic-reference")
    );
    harness.driver.assert_computer_sections_ordered();
    harness.driver.assert_computer_model_reference_layout();
    harness.teardown();
}

impl InputDriver {
    fn seed_computer_inventory(&mut self, model: &str) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.inventory = vec![
                ("Model identifier".into(), model.into()),
                ("Chip".into(), "Apple M5 Pro".into()),
                ("Architecture".into(), "arm64".into()),
                ("Physical CPU cores · maximum this boot".into(), "18".into()),
                ("Logical CPUs · maximum this boot".into(), "18".into()),
                (
                    "Installed memory".into(),
                    "48.00 GiB · 51539607552 bytes".into(),
                ),
                ("macOS version".into(), "26.0".into()),
                ("macOS build".into(), "25A000".into()),
                ("Darwin kernel".into(), "25.0.0".into()),
                ("VM page size · bytes".into(), "16384".into()),
                ("Boot time".into(), "2026-10-19 09:00:00 +0200".into()),
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
        let details = self
            .context
            .debug_bounds("computer-system-details")
            .unwrap();
        let reference = self
            .context
            .debug_bounds("computer-schematic-reference")
            .unwrap();
        assert!(model.bottom() <= details.top());
        assert!(details.bottom() <= reference.top());
    }
}

#[gpui::test]
fn computer_specifications_and_exact_system_details_fit_a_narrow_view(cx: &mut TestAppContext) {
    // Resize the installed model card and verify its values remain within the rendered panel.
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
        .assert_computer_specification_fits("computer-spec:Installed memory:48.00 GiB");
    assert!(
        harness
            .driver
            .reading_is_visible("computer-detail:macOS version:26.0")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("computer-detail:VM page size · bytes:16384")
    );
    harness.driver.assert_computer_detail_fits(
        "computer-detail:Installed memory:48.00 GiB · 51539607552 bytes",
    );
    harness
        .driver
        .assert_computer_detail_fits("computer-detail:Boot time:2026-10-19 09:00:00 +0200");
    harness.teardown();
}

#[gpui::test]
fn a_different_computer_model_keeps_its_reported_identity_without_a_guessed_generation(
    cx: &mut TestAppContext,
) {
    // Seed another captured identifier and retain a generic title alongside its actual fields.
    let mut harness = InputHarness::setup_route(cx, "computer");
    harness.driver.seed_distinct_computer_inventory();

    assert!(harness.driver.reading_is_visible("computer-model:This Mac"));
    assert!(
        harness
            .driver
            .reading_is_visible("computer-spec:Model identifier:Mac99,1")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("computer-spec:Chip:Apple M2 Max")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("computer-spec:Physical CPU cores:12")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("computer-spec:Installed memory:96.00 GiB")
    );
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
                ("Physical CPU cores · maximum this boot", "12"),
                ("Logical CPUs · maximum this boot", "12"),
                ("Installed memory", "96.00 GiB · 103079215104 bytes"),
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

    fn assert_computer_detail_fits(&mut self, selector: &'static str) {
        let panel = self
            .context
            .debug_bounds("computer-system-details")
            .unwrap();
        let reading = self.context.debug_bounds(selector).expect(selector);
        assert!(panel.left() <= reading.left());
        assert!(reading.right() <= panel.right());
        assert!(panel.top() <= reading.top());
        assert!(reading.bottom() <= panel.bottom());
        let viewport = self.context.update(|window, _| window.viewport_size());
        assert!(px(0.0) <= reading.left());
        assert!(reading.right() <= viewport.width);
        assert!(px(0.0) <= reading.top());
        assert!(reading.bottom() <= viewport.height);
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
fn computer_reference_links_open_the_traceable_source_with_keyboard_input(cx: &mut TestAppContext) {
    // Release Enter to activate the named reference through GPUI's fake platform.
    let mut harness = InputHarness::setup_route(cx, "computer");
    harness.driver.seed_computer_inventory("Mac17,8");
    assert!(
        harness
            .driver
            .reading_is_visible("computer-reference:Apple fan procedure")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("computer-reference:Apple exploded view")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("computer-reference:Counterpoint M5 Pro chip teardown")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("computer-reference:iFixit 2026 logic-board replacement")
    );

    harness.driver.focus_computer_reference();
    harness.driver.press_computer_reference_key();
    assert!(cx.opened_url().is_none());

    harness.driver.release_computer_reference_key();

    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://support.apple.com/en-us/125827")
    );
    harness.teardown();
}

impl InputDriver {
    fn focus_computer_reference(&mut self) {
        let bounds = self
            .context
            .debug_bounds("computer-reference:Apple logic-board service view")
            .unwrap();
        self.context.simulate_mouse_down(
            bounds.center(),
            gpui::MouseButton::Right,
            gpui::Modifiers::none(),
        );
        self.context.simulate_mouse_up(
            bounds.center(),
            gpui::MouseButton::Right,
            gpui::Modifiers::none(),
        );
        self.context.run_until_parked();
    }

    fn press_computer_reference_key(&mut self) {
        self.context.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        self.context.run_until_parked();
    }

    fn release_computer_reference_key(&mut self) {
        self.context.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        self.context.run_until_parked();
    }
}
