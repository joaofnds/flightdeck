use gpui::{Modifiers, TestAppContext};

use super::{InputDriver, InputHarness};
use crate::live_source::Request;

#[gpui::test]
fn hovering_reset_explains_the_default_until_the_pointer_leaves(cx: &mut TestAppContext) {
    // Observe delayed tooltip display and dismissal through the real hover path and test clock.
    let mut harness = InputHarness::setup(cx);
    harness.driver.activate_window();
    harness.driver.click_settings_control("navigation:settings");
    harness.driver.click_settings_control("animate-fans");
    assert!(!harness.driver.reading_is_visible("Reset to default (Off)"));

    harness
        .driver
        .hover_settings_control("restore-animate-fans");

    assert!(harness.driver.reading_is_visible("Reset to default (Off)"));
    harness.driver.hover_settings_control("animate-fans");
    assert!(!harness.driver.reading_is_visible("Reset to default (Off)"));
    harness.teardown();
}

#[gpui::test]
fn restoring_with_the_keyboard_returns_focus_to_the_animation_switch(cx: &mut TestAppContext) {
    // Reach the preceding reset arrow, then toggle with Space after focus returns to the switch.
    let mut harness = InputHarness::setup(cx);
    harness.driver.activate_window();
    harness.driver.click_settings_control("navigation:settings");
    harness.driver.press("tab space");
    harness.driver.release_settings_key("space");

    harness.driver.press("shift-tab enter");
    harness.driver.release_settings_key("enter");

    assert!(harness.driver.reading_is_visible("animate-fans:Off"));
    assert!(!harness.driver.reading_is_visible("restore-animate-fans"));
    harness.driver.press("space");
    harness.driver.release_settings_key("space");
    assert!(harness.driver.reading_is_visible("animate-fans:On"));
    harness.teardown();
}

#[gpui::test]
fn narrow_settings_keeps_the_description_above_the_switch(cx: &mut TestAppContext) {
    // At an intermediate width, leave readable text above the switch instead of beside it.
    let mut harness = InputHarness::setup(cx);
    harness
        .driver
        .context
        .simulate_resize(gpui::size(gpui::px(360.0), gpui::px(900.0)));
    harness.driver.context.run_until_parked();
    harness.driver.click_settings_control("navigation:settings");
    let description = harness
        .driver
        .context
        .debug_bounds("animate-fans-description")
        .unwrap();
    let toggle = harness.driver.context.debug_bounds("animate-fans").unwrap();

    assert!(description.size.width >= gpui::px(120.0));
    assert!(toggle.origin.y >= description.bottom());
    harness.teardown();
}

#[gpui::test]
fn settings_controls_fit_a_narrow_window(cx: &mut TestAppContext) {
    // Keep both controls inside the viewport when the settings row has to wrap.
    let mut harness = InputHarness::setup(cx);
    harness
        .driver
        .context
        .simulate_resize(gpui::size(gpui::px(320.0), gpui::px(900.0)));
    harness.driver.context.run_until_parked();
    harness.driver.click_settings_control("navigation:settings");
    harness.driver.click_settings_control("animate-fans");

    let toggle = harness.driver.context.debug_bounds("animate-fans").unwrap();
    let restore = harness
        .driver
        .context
        .debug_bounds("restore-animate-fans")
        .unwrap();

    assert!(toggle.right() <= gpui::px(320.0));
    assert!(restore.right() <= gpui::px(320.0));
    assert!(restore.bottom() <= gpui::px(900.0));
    harness
        .driver
        .click_settings_control("restore-animate-fans");
    assert!(harness.driver.reading_is_visible("animate-fans:Off"));
    harness.teardown();
}

#[gpui::test]
fn restoring_the_fan_default_returns_off_and_removes_the_restore_action(cx: &mut TestAppContext) {
    // Change the setting through its control, then restore only that choice through the page.
    let mut harness = InputHarness::setup(cx);
    harness.driver.click_settings_control("navigation:settings");
    assert!(!harness.driver.reading_is_visible("restore-animate-fans"));
    harness.driver.click_settings_control("animate-fans");
    let request = harness.driver.settings_resource_request();
    let restore = harness
        .driver
        .context
        .debug_bounds("restore-animate-fans")
        .unwrap();
    let label = harness
        .driver
        .context
        .debug_bounds("animate-fans-label")
        .unwrap();
    assert!(restore.left() >= label.right());
    assert!(restore.top() < label.bottom());
    assert!(restore.bottom() > label.top());

    harness
        .driver
        .click_settings_control("restore-animate-fans");

    assert!(harness.driver.reading_is_visible("animate-fans:Off"));
    assert!(!harness.driver.reading_is_visible("restore-animate-fans"));
    assert_eq!(harness.driver.settings_resource_request(), request);
    harness.teardown();
}

#[gpui::test]
fn unavailable_native_animation_keeps_the_figure_and_reports_its_failure(cx: &mut TestAppContext) {
    // GPUI's test window has no native AppKit host. Exercise the real setup failure path.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.context.deactivate_window();
    harness.driver.context.run_until_parked();
    harness.driver.activate_window();
    harness
        .driver
        .view
        .update(&mut harness.driver.context, |dashboard, cx| {
            dashboard.settings.animate_fans = true;
            let row = dashboard
                .rows
                .iter()
                .position(|row| matches!(row, crate::live_dashboard::Row::Hardware))
                .unwrap();
            dashboard.list.scroll_to(gpui::ListOffset {
                item_ix: row,
                offset_in_item: gpui::px(0.0),
            });
            cx.notify();
        });
    harness.driver.context.run_until_parked();
    harness
        .driver
        .view
        .update(&mut harness.driver.context, |dashboard, cx| {
            dashboard.tick(cx);
        });
    harness.driver.context.run_until_parked();

    assert!(
        harness
            .driver
            .reading_is_visible("hardware-animation-error")
    );
    assert!(
        harness
            .driver
            .view
            .read_with(&harness.driver.context, |dashboard, _| {
                dashboard.hardware_state.animation_statistics().is_none()
            })
    );
    harness.teardown();
}

#[gpui::test]
fn settings_remains_reachable_in_a_short_window(cx: &mut TestAppContext) {
    // Shrink below the route list's height and click the fixed Settings entry.
    let mut harness = InputHarness::setup(cx);
    for height in [500.0, 200.0] {
        harness
            .driver
            .context
            .simulate_resize(gpui::size(gpui::px(900.0), gpui::px(height)));
        harness.driver.context.run_until_parked();
        let bounds = harness
            .driver
            .context
            .debug_bounds("navigation:settings")
            .unwrap();
        assert!(bounds.origin.y >= gpui::px(0.0));
        assert!(bounds.bottom() <= gpui::px(height));

        harness.driver.click_settings_control("navigation:settings");

        assert!(harness.driver.reading_is_visible("settings-page"));
    }
    harness.teardown();
}

#[gpui::test]
fn settings_keeps_the_resource_request_and_returns_to_the_same_page(cx: &mut TestAppContext) {
    // Goal: Settings stays out of the reader. Method: open it and return through the sidebar.
    let mut harness = InputHarness::setup(cx);
    let request = harness.driver.settings_resource_request();
    let selection = harness.driver.accepted_selection();

    harness.driver.click_settings_control("navigation:settings");

    assert!(harness.driver.reading_is_visible("settings-page"));
    assert!(harness.driver.reading_is_visible("animate-fans:Off"));
    assert!(!harness.driver.reading_is_visible("reading-controls"));
    assert!(!harness.driver.reading_is_visible("time-range"));
    assert_eq!(harness.driver.settings_resource_request(), request);
    harness.driver.click_settings_control("navigation:energy");
    assert!(!harness.driver.reading_is_visible("settings-page"));
    assert!(harness.driver.reading_is_visible("reading-controls"));
    assert_eq!(harness.driver.settings_resource_request(), request);
    assert_eq!(harness.driver.accepted_selection(), selection);
    harness.teardown();
}

#[gpui::test]
fn command_comma_opens_settings_from_the_current_resource(cx: &mut TestAppContext) {
    // Goal: the app shortcut opens Settings. Method: dispatch the installed keyboard binding.
    let mut harness = InputHarness::setup(cx);
    harness.driver.activate_window();
    let request = harness.driver.settings_resource_request();

    harness.driver.press("cmd-,");

    assert!(harness.driver.reading_is_visible("settings-page"));
    assert_eq!(harness.driver.settings_resource_request(), request);
    harness.driver.click_settings_control("navigation:memory");
    assert!(!harness.driver.reading_is_visible("settings-page"));
    assert_eq!(harness.driver.settings_resource_request().0.route, "memory");
    harness.teardown();
}

#[gpui::test]
fn the_application_settings_action_opens_the_page(cx: &mut TestAppContext) {
    // Goal: the native menu opens Settings. Method: dispatch the menu's installed action.
    let mut harness = InputHarness::setup(cx);

    harness
        .driver
        .context
        .dispatch_action(crate::native_workflows::OpenSettings);

    assert!(harness.driver.reading_is_visible("settings-page"));
    harness.teardown();
}

#[gpui::test]
fn fan_animation_toggles_with_space_and_return_after_tabbing_to_it(cx: &mut TestAppContext) {
    // Goal: operate the checkbox without a pointer. Method: tab from Settings and press keys.
    let mut harness = InputHarness::setup(cx);
    harness.driver.activate_window();
    harness.driver.click_settings_control("navigation:settings");
    let request = harness.driver.settings_resource_request();

    harness.driver.press("tab space");
    harness.driver.release_settings_key("space");

    assert!(harness.driver.settings_animate_fans());
    assert!(harness.driver.reading_is_visible("animate-fans:On"));
    harness.driver.press("enter");
    harness.driver.release_settings_key("enter");
    assert!(!harness.driver.settings_animate_fans());
    assert!(harness.driver.reading_is_visible("animate-fans:Off"));
    assert_eq!(harness.driver.settings_resource_request(), request);
    harness.teardown();
}

#[gpui::test]
fn a_session_without_preferences_shows_that_the_animation_choice_cannot_be_saved(
    cx: &mut TestAppContext,
) {
    // Goal: avoid silently losing the choice. Method: click the checkbox without a store.
    let mut harness = InputHarness::setup(cx);
    harness.driver.click_settings_control("navigation:settings");

    harness.driver.click_settings_control("animate-fans");

    assert!(harness.driver.settings_animate_fans());
    assert!(harness.driver.reading_is_visible("animate-fans:On"));
    assert!(harness.driver.reading_is_visible("settings-notice"));
    assert!(harness.driver.settings_notice().is_some());
    harness.teardown();
}

impl InputDriver {
    fn hover_settings_control(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).expect(selector);
        self.hover_position(bounds.center());
        self.context.run_until_parked();
        self.context
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        self.context.run_until_parked();
    }

    fn release_settings_key(&mut self, key: &str) {
        self.context.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse(key).unwrap(),
        });
        self.context.run_until_parked();
    }

    fn click_settings_control(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).expect(selector);
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }

    fn settings_resource_request(&self) -> (Request, u64) {
        self.view.read_with(&self.context, |dashboard, _| {
            (dashboard.request.clone(), dashboard.generation)
        })
    }

    fn settings_animate_fans(&self) -> bool {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.settings.animate_fans
        })
    }

    fn settings_notice(&self) -> Option<String> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.settings.notice.clone()
        })
    }
}
