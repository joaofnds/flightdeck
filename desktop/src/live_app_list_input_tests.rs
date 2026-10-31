use gpui::Modifiers;
use serde_json::json;

use super::{AppEpoch, AppHoverDriver, AppHoverHarness};
use crate::live_source::Update;

/// Three Apps beside the open one, whose CPU readings order them apart from their names.
const APPS: [(&str, &str, Option<f64>); 3] = [
    ("Alpha", "51", Some(5.0)),
    ("Bravo", "52", None),
    ("Charlie", "53", Some(30.0)),
];

#[gpui::test]
fn clicking_the_cpu_heading_lists_the_apps_by_cpu_the_most_first(cx: &mut gpui::TestAppContext) {
    // Goal: a click on the CPU heading of the open App picker orders its Apps by CPU, the most
    // first, with the App that has no reading last.
    // Method: list three Apps whose names order them otherwise, click the heading, and read the
    // order the names stand in on the screen.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_apps(&APPS);

    harness.driver.click_heading("CPU %");

    assert_eq!(harness.driver.listed(), ["Charlie", "Alpha", "Bravo"]);
    harness.teardown();
}

#[gpui::test]
fn a_second_click_on_the_cpu_heading_lists_the_apps_the_least_first(cx: &mut gpui::TestAppContext) {
    // Goal: a second click on the heading the Apps are sorted by turns the order around, and the
    // App with no reading stays last.
    // Method: click the CPU heading twice and read the order of the names on the screen.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_apps(&APPS);
    harness.driver.click_heading("CPU %");

    harness.driver.click_heading("CPU %");

    assert_eq!(harness.driver.listed(), ["Alpha", "Charlie", "Bravo"]);
    harness.teardown();
}

#[gpui::test]
fn a_click_on_the_app_heading_lists_the_apps_by_name_from_a(cx: &mut gpui::TestAppContext) {
    // Goal: the App heading sorts by name from A whatever column the Apps were sorted by.
    // Method: sort by CPU, click the App heading, and read the order of the names on the screen.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_apps(&APPS);
    harness.driver.click_heading("CPU %");

    harness.driver.click_heading("App");

    assert_eq!(harness.driver.listed(), ["Alpha", "Bravo", "Charlie"]);
    harness.teardown();
}

#[gpui::test]
fn typing_in_the_app_search_lists_only_the_apps_whose_name_contains_it(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: text typed in the open App picker's search field keeps only the Apps whose name
    // contains it, in the order the list had.
    // Method: list three Apps, type two letters that two of the names contain, and read which
    // names the screen shows and in what order.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_apps(&APPS);

    harness.driver.search_apps("ha");

    assert_eq!(harness.driver.listed(), ["Alpha", "Charlie"]);
    harness.teardown();
}

#[gpui::test]
fn typing_right_after_opening_the_app_picker_searches_its_apps(cx: &mut gpui::TestAppContext) {
    // Goal: the App picker's search field takes keystrokes as soon as a click opens the picker,
    // with no click in the field.
    // Method: close the picker the harness opened, open it again with a click on the App
    // select, type two letters that two of three names contain, and read which names the screen
    // shows.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_apps(&APPS);
    harness.driver.click_app_select();

    harness.driver.click_app_select();
    harness.driver.type_text("ha");

    assert_eq!(harness.driver.listed(), ["Alpha", "Charlie"]);
    harness.teardown();
}

#[gpui::test]
fn typing_right_after_reopening_the_app_picker_starts_a_new_search(cx: &mut gpui::TestAppContext) {
    // Goal: text typed right after the App picker opens again replaces the search it closed
    // with, so a person who reopens it to find another App lists that App.
    // Method: search for two letters, close and reopen the picker with clicks on the App select,
    // type the start of another name, and read which names the screen shows.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_apps(&APPS);
    harness.driver.search_apps("ha");
    harness.driver.click_app_select();
    harness.driver.click_app_select();

    harness.driver.type_text("br");

    assert_eq!(harness.driver.listed(), ["Bravo"]);
    harness.teardown();
}

#[gpui::test]
fn the_app_search_finds_an_app_listed_after_the_first_ten(cx: &mut gpui::TestAppContext) {
    // Goal: a search looks through every App of a second, not only the ten the picker lists
    // before it.
    // Method: list 600 Apps, search for the last one's name, and look for it on the screen.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_numbered_apps(600);

    harness.driver.search_apps("Echo 599");

    assert!(harness.driver.context.debug_bounds("Echo 599").is_some());
    harness.teardown();
}

#[gpui::test]
fn the_app_picker_lists_ten_of_600_apps(cx: &mut gpui::TestAppContext) {
    // Goal: the open App picker lists ten Apps at most, however many the second holds.
    // Method: list 600 Apps by name, then look for the tenth and the eleventh on the screen, and
    // for the heading of the table below the picker, which shows the list was drawn past it.
    let mut harness = AppHoverHarness::setup(cx);

    harness.driver.seed_numbered_apps(600);

    let context = &mut harness.driver.context;
    assert!(context.debug_bounds("Echo 009").is_some());
    assert!(context.debug_bounds("Echo 010").is_none());
    assert!(
        context
            .debug_bounds("app-column:executables:Process group")
            .is_some()
    );
    harness.teardown();
}

#[gpui::test]
fn a_search_more_than_ten_app_names_contain_lists_ten(cx: &mut gpui::TestAppContext) {
    // Goal: a search that more than ten App names contain lists ten of those Apps at most.
    // Method: list 600 Apps by name, search for text the first hundred names contain, then look
    // for the tenth and the eleventh of them on the screen, and for the heading of the table
    // below the picker, which shows the list was drawn past it.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_numbered_apps(600);

    harness.driver.search_apps("Echo 0");

    let context = &mut harness.driver.context;
    assert!(context.debug_bounds("Echo 009").is_some());
    assert!(context.debug_bounds("Echo 010").is_none());
    assert!(
        context
            .debug_bounds("app-column:executables:Process group")
            .is_some()
    );
    harness.teardown();
}

#[gpui::test]
fn the_app_picker_marks_the_open_app_and_no_other(cx: &mut gpui::TestAppContext) {
    // Goal: the App picker marks the row of the App the page shows as selected, and no other.
    // Method: list three Apps beside the open one and look for the selected mark on the open
    // App's name and on another App's.
    let mut harness = AppHoverHarness::setup(cx);

    harness.driver.seed_apps(&APPS);

    let context = &mut harness.driver.context;
    assert!(context.debug_bounds("selected:Example App").is_some());
    assert!(context.debug_bounds("selected:Alpha").is_none());
    harness.teardown();
}

#[gpui::test]
fn the_app_picker_draws_its_apps_as_the_inside_app_table_draws_its_process_groups(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: the App picker lists its Apps in the table that lists the process groups inside the
    // App, with a heading as tall, rows as tall and a name column as wide.
    // Method: list three Apps beside the open App's two process groups, then measure the name
    // heading and the first two names of each table on the screen and compare the two.
    let mut harness = AppHoverHarness::setup(cx);

    harness.driver.seed_apps(&APPS);

    let picker = harness
        .driver
        .table_geometry("app-column:apps:App", ["Alpha", "Bravo"]);
    let groups = harness.driver.table_geometry(
        "app-column:executables:Process group",
        ["Accepted worker", "Accepted helper"],
    );
    assert_eq!(picker, groups);
    harness.teardown();
}

#[gpui::test]
fn the_last_app_the_picker_lists_closes_its_panel(cx: &mut gpui::TestAppContext) {
    // Goal: the App picker's panel closes under the last App it lists, and under no App above.
    // Method: list three Apps, search for two letters the first and the third names contain,
    // and look for the panel's bottom edge on the row of each.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_apps(&APPS);

    harness.driver.search_apps("ha");

    assert!(harness.driver.closes_app_panel("Charlie"));
    assert!(!harness.driver.closes_app_panel("Alpha"));
    harness.teardown();
}

#[gpui::test]
fn a_search_no_app_name_contains_closes_the_picker_panel_under_its_heading(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: when no App's name contains the search, the App picker's panel closes under its
    // heading, the only row left in it.
    // Method: list three Apps, search for letters none of the names contains, and look for the
    // panel's bottom edge on the heading.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_apps(&APPS);

    harness.driver.search_apps("zzz");

    assert!(
        harness
            .driver
            .context
            .debug_bounds("panel-bottom:app-columns-apps")
            .is_some()
    );
    harness.teardown();
}

#[gpui::test]
fn the_last_process_group_closes_the_inside_app_panel(cx: &mut gpui::TestAppContext) {
    // Goal: the panel of the process groups inside the App closes under the last group it lists,
    // and under no group above.
    // Method: show the open App's two process groups and look for the panel's bottom edge on the
    // row of each.
    let mut harness = AppHoverHarness::setup(cx);

    harness.driver.seed_apps(&APPS);

    let context = &mut harness.driver.context;
    assert!(context.debug_bounds("panel-bottom:executable-1").is_some());
    assert!(context.debug_bounds("panel-bottom:executable-0").is_none());
    harness.teardown();
}

#[gpui::test]
fn opening_the_app_picker_leaves_the_process_groups_where_they_were_scrolled(
    cx: &mut gpui::TestAppContext,
) {
    // Goal: the App picker, whose table is narrower than the process groups' table below it,
    // leaves that table where a person scrolled it sideways.
    // Method: in a window wider than the picker's table and narrower than the process groups',
    // close the picker, turn the wheel sideways past the end of the process groups, open the
    // picker, and see a group's name stay where the wheel left it.
    let mut harness = AppHoverHarness::setup(cx);
    harness.driver.seed_apps(&APPS);
    harness
        .driver
        .context
        .simulate_resize(gpui::size(gpui::px(1000.0), gpui::px(1200.0)));
    harness.driver.context.run_until_parked();
    harness.driver.click_app_select();
    let unscrolled = harness.driver.name_left("Accepted worker");
    harness.driver.wheel_sideways("Accepted worker", -600.0);
    let scrolled = harness.driver.name_left("Accepted worker");

    harness.driver.click_app_select();

    assert!(scrolled < unscrolled);
    assert_eq!(harness.driver.name_left("Accepted worker"), scrolled);
    harness.teardown();
}

/// Where a table draws its name heading and its first two rows' names, as distances that do not
/// depend on where the table stands on the page.
#[derive(Debug, PartialEq)]
struct TableGeometry {
    heading_width: gpui::Pixels,
    name_width: gpui::Pixels,
    name_height: gpui::Pixels,
    heading_to_first_row: gpui::Pixels,
    first_row_to_second: gpui::Pixels,
}

impl AppHoverDriver {
    /// The geometry of the table whose name heading has the selector `heading` and whose rows
    /// have the names `names`, in either order.
    fn table_geometry(&mut self, heading: &'static str, names: [&'static str; 2]) -> TableGeometry {
        let heading = self.context.debug_bounds(heading).expect(heading);
        let mut rows = names.map(|name| self.context.debug_bounds(name).expect(name));
        rows.sort_by(|first, second| f32::from(first.top()).total_cmp(&f32::from(second.top())));
        let [first, second] = rows;

        TableGeometry {
            heading_width: heading.size.width,
            name_width: first.size.width,
            name_height: first.size.height,
            heading_to_first_row: first.center().y - heading.center().y,
            first_row_to_second: second.center().y - first.center().y,
        }
    }

    fn seed_apps(&mut self, apps: &[(&str, &str, Option<f64>)]) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = AppEpoch::Accepted.snapshot(dashboard.generation);
            let page = snapshot.apps.as_mut().unwrap().legacy_mut().unwrap();
            let open = page["apps"][0].clone();
            let listed: Vec<_> = apps
                .iter()
                .map(|&(name, bytes, cpu)| {
                    let mut app = open.clone();
                    app["display_name"] = json!(name);
                    app["key"]["bytes_hex"] = json!(bytes);
                    app["key"]["display"] = json!(name);
                    app["cpu_percent_one_core"] = json!(cpu);
                    app
                })
                .collect();
            page["apps"] = json!(listed);
            page["page"]["total"] = json!(apps.len());
            assert!(dashboard.take_update(Update::Ready(snapshot)));
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// Lists `count` Apps named Echo 000, Echo 001 and on, none with a CPU reading.
    fn seed_numbered_apps(&mut self, count: usize) {
        let names: Vec<String> = (0..count).map(|index| format!("Echo {index:03}")).collect();
        let keys: Vec<String> = (0..count)
            .map(|index| format!("{:04x}", 0x1000 + index))
            .collect();
        let apps: Vec<_> = names
            .iter()
            .zip(&keys)
            .map(|(name, key)| (name.as_str(), key.as_str(), None))
            .collect();
        self.seed_apps(&apps);
    }

    /// Clicks the App select, which opens the App picker or closes it.
    fn click_app_select(&mut self) {
        let bounds = self.context.debug_bounds("app-select").expect("app-select");
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }

    /// Turns the wheel sideways by `distance` over the middle of the name `name`, which must be
    /// on the screen.
    fn wheel_sideways(&mut self, name: &'static str, distance: f32) {
        let bounds = self.context.debug_bounds(name).expect(name);
        self.context.simulate_event(gpui::ScrollWheelEvent {
            position: bounds.center(),
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(distance), gpui::px(0.0))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        self.context.run_until_parked();
    }

    /// Whether the App picker's panel closes under the row of `name`, one of `APPS`.
    fn closes_app_panel(&mut self, name: &str) -> bool {
        let index = APPS
            .iter()
            .position(|&(listed, _, _)| listed == name)
            .expect(name);
        let selector: &'static str = format!("panel-bottom:app-choice-{index}").leak();
        self.context.debug_bounds(selector).is_some()
    }

    /// Where the name `name` starts on the screen, from its left.
    fn name_left(&mut self, name: &'static str) -> gpui::Pixels {
        self.context.debug_bounds(name).expect(name).left()
    }

    fn click_heading(&mut self, heading: &str) {
        let selector: &'static str = format!("app-column:apps:{heading}").leak();
        let bounds = self.context.debug_bounds(selector).expect(selector);
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }

    /// Clicks the App picker's search field and types `text` into it.
    fn search_apps(&mut self, text: &str) {
        let bounds = self.context.debug_bounds("app-search").expect("app-search");
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.type_text(text);
    }

    /// Types `text` into whatever has the keyboard's focus.
    fn type_text(&mut self, text: &str) {
        self.context.simulate_input(text);
        self.context.run_until_parked();
    }

    /// The names of `APPS` the picker draws, in its order, top first.
    fn listed(&mut self) -> Vec<&'static str> {
        let mut tops: Vec<_> = APPS
            .iter()
            .filter_map(|&(name, _, _)| Some((self.context.debug_bounds(name)?.top(), name)))
            .collect();
        tops.sort_by(|first, second| f32::from(first.0).total_cmp(&f32::from(second.0)));
        tops.into_iter().map(|(_, name)| name).collect()
    }
}
