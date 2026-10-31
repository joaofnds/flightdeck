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

impl AppHoverDriver {
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
