use gpui::{Bounds, Pixels, TestAppContext, px};

use super::{InputDriver, InputHarness};

/// Where each row of a page landed, in order, and where each chart card the page draws landed.
#[derive(Debug, PartialEq)]
struct PageLayout {
    rows: Vec<(String, Bounds<Pixels>)>,
    charts: Vec<(String, Bounds<Pixels>)>,
}

macro_rules! page_keeps_layout_case {
    ($name:ident, $route:literal) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Goal: a page a launch can open, which is the page last shown, draws its rows and
            // charts where its first reading leaves them, so that reading moves nothing. Method:
            // open the page with its read still owed, lay it out, then hand it its reading and
            // lay it out again.
            let mut harness = InputHarness::open_route(cx, $route);

            let loading = harness.driver.page_layout(1320.0);
            harness.driver.seed_accepted_selection();
            let loaded = harness.driver.page_layout_like(1320.0, &loading);

            assert_eq!(loading, loaded);
            harness.teardown();
        }
    };
}

page_keeps_layout_case!(
    overview_keeps_its_layout_when_its_first_reading_arrives,
    "overview"
);
page_keeps_layout_case!(cpu_keeps_its_layout_when_its_first_reading_arrives, "cpu");
page_keeps_layout_case!(gpu_keeps_its_layout_when_its_first_reading_arrives, "gpu");
page_keeps_layout_case!(
    memory_keeps_its_layout_when_its_first_reading_arrives,
    "memory"
);
page_keeps_layout_case!(
    energy_keeps_its_layout_when_its_first_reading_arrives,
    "energy"
);
page_keeps_layout_case!(
    storage_keeps_its_layout_when_its_first_reading_arrives,
    "storage"
);
page_keeps_layout_case!(
    network_keeps_its_layout_when_its_first_reading_arrives,
    "network"
);
page_keeps_layout_case!(
    battery_keeps_its_layout_when_its_first_reading_arrives,
    "battery"
);

#[gpui::test]
fn overview_keeps_its_layout_when_a_full_first_reading_arrives(cx: &mut TestAppContext) {
    // Goal: readings with every ranking and App history in them take the room the loading
    // page held for them. Method: lay the Overview out before its first reading, then with a
    // reading that fills each section.
    let mut harness = InputHarness::open_route(cx, "overview");

    let loading = harness.driver.page_layout(1320.0);
    harness.driver.seed_overview_histories(1320.0);
    let loaded = harness.driver.page_layout_like(1320.0, &loading);

    assert_eq!(loading, loaded);
    harness.teardown();
}

macro_rules! app_chart_keeps_layout_case {
    ($name:ident, $route:literal, $metric:ident) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Goal: a page's App chart holds the room of the Apps its readings name before the
            // first reading names them. Method: open the page with its read still owed, then
            // hand it a reading that holds that App history.
            use crate::live_app_attribution_history::Metric;
            let mut harness = InputHarness::open_route(cx, $route);

            let loading = harness.driver.page_layout(1320.0);
            harness.driver.seed_attribution(Metric::$metric);
            let loaded = harness.driver.page_layout_like(1320.0, &loading);

            assert_eq!(loading, loaded);
            harness.teardown();
        }
    };
}

app_chart_keeps_layout_case!(
    the_cpu_app_chart_keeps_its_size_when_its_apps_arrive,
    "cpu",
    Cpu
);
app_chart_keeps_layout_case!(
    the_energy_app_chart_keeps_its_size_when_its_apps_arrive,
    "energy",
    Power
);

#[gpui::test]
fn the_memory_app_chart_keeps_its_size_when_its_eight_leading_apps_arrive(cx: &mut TestAppContext) {
    // Goal: the resident memory chart holds the room of the most Apps it can name, the eight
    // leading Apps and Other. Method: open the Memory page with its read still owed, then hand it
    // a reading whose resident history names eight Apps and Other.
    let mut harness = InputHarness::open_route(cx, "memory");

    let loading = harness.driver.page_layout(1320.0);
    harness.driver.seed_interval_resident_leaders();
    let loaded = harness.driver.page_layout_like(1320.0, &loading);

    assert_eq!(loading, loaded);
    harness.teardown();
}

#[gpui::test]
fn the_apps_page_keeps_what_it_drew_when_its_first_reading_arrives(cx: &mut TestAppContext) {
    // Goal: only the reading names the selected App's executables, so the Apps page draws the
    // rows above them and the reading adds rows below, moving none it drew. Method: lay the
    // page out before and after its first reading, then compare the rows both frames hold.
    let mut harness = InputHarness::open_route(cx, "apps");

    let loading = harness.driver.page_layout(1320.0);
    harness.driver.seed_accepted_selection();
    let loaded = harness.driver.page_layout_like(1320.0, &loading);

    let keys: Vec<&str> = loading.rows.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "summary",
            "app-selector",
            "app-executable-heading",
            "app-columns-executables"
        ]
    );
    assert_eq!(loading.rows, loaded.rows[..loading.rows.len()]);
    harness.teardown();
}

macro_rules! cpu_panels_keep_layout_case {
    ($name:ident, $width:literal) => {
        #[gpui::test]
        fn $name(cx: &mut TestAppContext) {
            // Goal: the CPU page's panels of each logical CPU hold the size their readings give
            // them before those readings arrive, from no fact about the Mac. Method: lay the page
            // out before its first reading, then with readings of 18 logical CPUs in two levels.
            let mut harness = InputHarness::open_route(cx, "cpu");

            let loading = harness.driver.page_layout($width);
            harness.driver.seed_cpu_readings();
            let loaded = harness.driver.page_layout_like($width, &loading);

            assert_eq!(loading, loaded);
            harness.teardown();
        }
    };
}

cpu_panels_keep_layout_case!(the_cpu_panels_keep_their_size_side_by_side, 1320.0);
cpu_panels_keep_layout_case!(the_cpu_panels_keep_their_size_stacked, 1099.0);

#[gpui::test]
fn a_page_says_it_is_loading_in_place_of_each_reading(cx: &mut TestAppContext) {
    // Goal: before its first reading the Overview says each reading is loading rather than
    // unavailable, and the reading replaces that. Method: open the Overview with its read still
    // owed, look for each kind of reading's loading text, then hand it its reading.
    let loading = [
        "summary-card:CPU:Loading",
        "overview-context:soc:Loading",
        "utilization:GPU:Loading",
        "chart-caption:live-memory-pressure:Loading",
    ];
    let mut harness = InputHarness::open_route(cx, "overview");

    harness.driver.page_layout(1320.0);
    for selector in loading {
        assert!(harness.driver.reading_is_visible(selector), "{selector}");
    }
    assert!(
        !harness
            .driver
            .reading_is_visible("overview-context:soc:Unavailable")
    );
    harness.driver.seed_accepted_selection();
    harness.driver.page_layout(1320.0);

    for selector in loading {
        assert!(!harness.driver.reading_is_visible(selector), "{selector}");
    }
    harness.teardown();
}

#[gpui::test]
fn the_battery_page_says_it_is_loading_in_place_of_its_details(cx: &mut TestAppContext) {
    // Goal: the battery's details say they are loading before the page's first reading, as its
    // summary cards do. Method: open the Battery page with its read still owed.
    let mut harness = InputHarness::open_route(cx, "battery");

    harness.driver.page_layout(1320.0);

    assert!(
        harness
            .driver
            .reading_is_visible("summary-card:Health:Loading")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("overview-context:health:Loading")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("overview-context:cycles:Loading")
    );
    harness.teardown();
}

impl InputDriver {
    /// Lays the page out in a window `width` wide and tall enough to hold every row, then reads
    /// where each row and each chart card the page draws landed. Every row must be laid out.
    fn page_layout(&mut self, width: f32) -> PageLayout {
        let charts = self.view.read_with(&self.context, |dashboard, _| {
            dashboard
                .panels
                .iter()
                .take(64)
                .map(|panel| panel.id.clone())
                .collect()
        });
        self.page_layout_of(width, charts)
    }

    /// The same, reading where the chart cards of `like` landed, each of which the page must
    /// draw.
    fn page_layout_like(&mut self, width: f32, like: &PageLayout) -> PageLayout {
        let charts = like.charts.iter().map(|(id, _)| id.clone()).collect();
        self.page_layout_of(width, charts)
    }

    fn page_layout_of(&mut self, width: f32, charts: Vec<String>) -> PageLayout {
        self.context
            .simulate_resize(gpui::size(px(width), px(7000.0)));
        self.render_pending();

        let keys: Vec<String> = self.view.read_with(&self.context, |dashboard, _| {
            dashboard
                .rows
                .iter()
                .map(crate::live_dashboard::Row::layout_key)
                .collect()
        });
        let rows = keys
            .into_iter()
            .map(|key| {
                let bounds = self.bounds_of(format!("row:{key}"));
                (key, bounds)
            })
            .collect();
        let charts = charts
            .into_iter()
            .map(|id| {
                let bounds = self.bounds_of(format!("chart-card:{id}"));
                (id, bounds)
            })
            .collect();
        PageLayout { rows, charts }
    }

    fn bounds_of(&mut self, selector: String) -> Bounds<Pixels> {
        let selector: &'static str = Box::leak(selector.into_boxed_str());
        self.context
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not laid out"))
    }
}
