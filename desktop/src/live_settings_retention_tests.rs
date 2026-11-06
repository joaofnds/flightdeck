//! The Recording section of Settings, driven through the page against a recording's real file.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::recording::private::{PrivateDirectory, create_private_dir};
use gpui::{TestAppContext, px, size};
use persistence::retention::Period;

use super::InputHarness;
use crate::live_source::Worker;

static NEXT: AtomicU64 = AtomicU64::new(0);

const DAY_SECONDS: u64 = 86_400;
const TEN_YEARS_SECONDS: u64 = 10 * 366 * DAY_SECONDS;

/// A recording's directory holding what the setting needs.
struct RetentionRoot {
    path: PathBuf,
}

impl RetentionRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "flightdeck-settings-retention-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        create_private_dir(&path).unwrap();
        create_private_dir(&path.join("retention")).unwrap();

        Self { path }
    }

    fn holding(seconds: u64) -> Self {
        let root = Self::new();
        root.save(seconds);

        root
    }

    fn save(&self, seconds: u64) {
        Period::from_seconds(seconds)
            .unwrap()
            .write(&self.directory())
            .unwrap();
    }

    fn directory(&self) -> PrivateDirectory {
        PrivateDirectory::open(&self.path).unwrap()
    }

    fn period_seconds(&self) -> u64 {
        Period::read(&self.directory()).seconds()
    }

    fn page(&self, cx: &mut TestAppContext) -> InputHarness {
        let worker = Worker::start(self.path.clone()).unwrap();
        let mut harness = InputHarness::open_route_through(cx, "energy", worker);
        harness.driver.click_control("navigation:settings");

        harness
    }
}

impl Drop for RetentionRoot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).unwrap();
    }
}

#[gpui::test]
fn a_recording_with_no_period_keeps_its_readings_for_one_day(cx: &mut TestAppContext) {
    // Goal: the page states the default the persister and the readers apply.
    // Method: open the page over a recording that holds no period.
    let root = RetentionRoot::new();

    let mut harness = root.page(cx);

    assert!(harness.driver.reading_is_visible("retention:1:Days"));
    harness.teardown();
}

#[gpui::test]
fn opening_settings_shows_the_period_the_recording_holds(cx: &mut TestAppContext) {
    // Goal: the recording's file is the setting's one copy, so a page opened after it changed
    // shows what it now holds, in the largest unit that states it.
    // Method: open the page, close it, change the file, and open the page again.
    let root = RetentionRoot::holding(5_400);
    let mut harness = root.page(cx);
    assert!(harness.driver.reading_is_visible("retention:90:Minutes"));
    harness.driver.click_control("navigation:overview");
    root.save(2 * 3_600);

    harness.driver.click_control("navigation:settings");

    assert!(harness.driver.reading_is_visible("retention:2:Hours"));
    harness.teardown();
}

#[gpui::test]
fn raising_the_amount_saves_a_longer_period(cx: &mut TestAppContext) {
    // Goal: the choice reaches what the persister and the readers read.
    // Method: raise the amount through a click and read the recording's period.
    let root = RetentionRoot::new();
    let mut harness = root.page(cx);

    harness.driver.click_control("retention-increase");

    assert!(harness.driver.reading_is_visible("retention:2:Days"));
    assert_eq!(root.period_seconds(), 2 * DAY_SECONDS);
    harness.teardown();
}

#[gpui::test]
fn lowering_the_amount_saves_a_shorter_period(cx: &mut TestAppContext) {
    // Goal: the user can shorten the period, which is what deletes readings sooner.
    // Method: lower the amount through a click and read the recording's period.
    let root = RetentionRoot::holding(3 * 3_600);
    let mut harness = root.page(cx);

    harness.driver.click_control("retention-decrease");

    assert!(harness.driver.reading_is_visible("retention:2:Hours"));
    assert_eq!(root.period_seconds(), 2 * 3_600);
    harness.teardown();
}

#[gpui::test]
fn a_minute_is_the_shortest_period_the_page_saves(cx: &mut TestAppContext) {
    // Goal: the page cannot leave a period no reader or persister accepts.
    // Method: lower the amount from one minute.
    let root = RetentionRoot::holding(60);
    let mut harness = root.page(cx);

    harness.driver.click_control("retention-decrease");

    assert!(harness.driver.reading_is_visible("retention:1:Minutes"));
    assert_eq!(root.period_seconds(), 60);
    harness.teardown();
}

#[gpui::test]
fn ten_years_is_the_longest_period_the_page_saves(cx: &mut TestAppContext) {
    // Goal: the page cannot leave a period no reader or persister accepts, which they would read
    // as the default and so delete almost everything.
    // Method: raise the amount from the longest period.
    let root = RetentionRoot::holding(TEN_YEARS_SECONDS);
    let mut harness = root.page(cx);

    harness.driver.click_control("retention-increase");

    assert!(harness.driver.reading_is_visible("retention:3660:Days"));
    assert_eq!(root.period_seconds(), TEN_YEARS_SECONDS);
    harness.teardown();
}

#[gpui::test]
fn changing_the_unit_states_the_same_period(cx: &mut TestAppContext) {
    // Goal: changing how a period is stated deletes nothing.
    // Method: change a day to hours, leave the page, come back, and read the recording's period.
    let root = RetentionRoot::new();
    let mut harness = root.page(cx);

    harness.driver.click_control("retention-unit:Hours");
    harness.driver.click_control("navigation:overview");
    harness.driver.click_control("navigation:settings");

    assert!(harness.driver.reading_is_visible("retention:24:Hours"));
    assert_eq!(root.period_seconds(), DAY_SECONDS);
    harness.teardown();
}

#[gpui::test]
fn changing_to_a_unit_that_cannot_state_the_period_lengthens_it(cx: &mut TestAppContext) {
    // Goal: a change of unit never shortens the period, so it never deletes a reading.
    // Method: change ninety minutes to hours.
    let root = RetentionRoot::holding(5_400);
    let mut harness = root.page(cx);

    harness.driver.click_control("retention-unit:Hours");

    assert!(harness.driver.reading_is_visible("retention:2:Hours"));
    assert_eq!(root.period_seconds(), 2 * 3_600);
    harness.teardown();
}

#[gpui::test]
fn a_refused_save_says_so_and_shows_the_period_the_recording_keeps(cx: &mut TestAppContext) {
    // Goal: the page never claims a period the recording does not hold.
    // Method: take away the directory the period lives in, and raise the amount.
    let root = RetentionRoot::new();
    let mut harness = root.page(cx);
    std::fs::remove_dir(root.path.join("retention")).unwrap();

    harness.driver.click_control("retention-increase");
    create_private_dir(&root.path.join("retention")).unwrap();

    assert!(harness.driver.reading_is_visible("retention:1:Days"));
    assert!(harness.driver.reading_is_visible("retention-notice"));
    harness.teardown();
}

#[gpui::test]
fn a_save_that_succeeds_clears_the_notice_of_one_that_was_refused(cx: &mut TestAppContext) {
    // Goal: a notice lasts only as long as what it reports.
    // Method: refuse a save, give the directory back, and save again.
    let root = RetentionRoot::new();
    let mut harness = root.page(cx);
    std::fs::remove_dir(root.path.join("retention")).unwrap();
    harness.driver.click_control("retention-increase");
    create_private_dir(&root.path.join("retention")).unwrap();

    harness.driver.click_control("retention-increase");

    assert!(harness.driver.reading_is_visible("retention:2:Days"));
    assert!(!harness.driver.reading_is_visible("retention-notice"));
    harness.teardown();
}

#[gpui::test]
fn opening_settings_again_clears_the_notice_of_a_refused_save(cx: &mut TestAppContext) {
    // Goal: a notice lasts only as long as what it reports.
    // Method: refuse a save, give the directory back, leave the page, and come back.
    let root = RetentionRoot::new();
    let mut harness = root.page(cx);
    std::fs::remove_dir(root.path.join("retention")).unwrap();
    harness.driver.click_control("retention-increase");
    create_private_dir(&root.path.join("retention")).unwrap();

    harness.driver.click_control("navigation:overview");
    harness.driver.click_control("navigation:settings");

    assert!(!harness.driver.reading_is_visible("retention-notice"));
    harness.teardown();
}

#[gpui::test]
fn a_period_no_unit_states_reads_as_the_next_whole_minute(cx: &mut TestAppContext) {
    // Goal: the page never states a period shorter than the one the recording keeps.
    // Method: hold ninety seconds, which only a file edited by hand can, and open the page.
    let root = RetentionRoot::holding(90);

    let mut harness = root.page(cx);

    assert!(harness.driver.reading_is_visible("retention:2:Minutes"));
    harness.teardown();
}

#[gpui::test]
fn the_steppers_and_the_units_are_large_enough_to_touch(cx: &mut TestAppContext) {
    // Goal: a control of the section can be hit by touch as well as by a pointer.
    // Method: measure one of each kind of control, which the section builds the same way.
    let root = RetentionRoot::new();
    let mut harness = root.page(cx);

    let stepper = harness
        .driver
        .context
        .debug_bounds("retention-decrease")
        .unwrap();
    let unit = harness
        .driver
        .context
        .debug_bounds("retention-unit:Minutes")
        .unwrap();

    assert!(stepper.size.width >= px(44.0));
    assert!(stepper.size.height >= px(44.0));
    assert!(unit.size.width >= px(44.0));
    assert!(unit.size.height >= px(44.0));
    harness.teardown();
}

#[gpui::test]
fn the_amount_can_be_raised_from_the_keyboard(cx: &mut TestAppContext) {
    // Goal: every control of the section works without a pointer.
    // Method: tab past the fan switch and the decrease button, and press Enter.
    let root = RetentionRoot::new();
    let mut harness = root.page(cx);

    harness.driver.press("tab tab tab enter");
    harness.driver.release_settings_key("enter");

    assert!(harness.driver.reading_is_visible("retention:2:Days"));
    assert_eq!(root.period_seconds(), 2 * DAY_SECONDS);
    harness.teardown();
}

#[gpui::test]
fn the_controls_fit_a_narrow_window(cx: &mut TestAppContext) {
    // Goal: every control stays inside a window at its narrowest.
    // Method: open the page at 320 points and measure the last control of the section.
    let root = RetentionRoot::new();
    let mut harness = root.page(cx);

    harness
        .driver
        .context
        .simulate_resize(size(px(320.0), px(900.0)));
    harness.driver.context.run_until_parked();

    let days = harness
        .driver
        .context
        .debug_bounds("retention-unit:Days")
        .unwrap();
    assert!(days.right() <= px(320.0));
    harness.teardown();
}
