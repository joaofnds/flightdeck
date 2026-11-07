use serde_json::{Value, json};

use super::{AppColumn, AppOrder, app_order};

fn apps_by_cpu() -> [Value; 4] {
    [
        json!({"display_name": "b", "cpu_percent_one_core": 2.0}),
        json!({"display_name": "a", "cpu_percent_one_core": 5.0}),
        json!({"display_name": "c", "cpu_percent_one_core": null}),
        json!({"display_name": "d", "cpu_percent_one_core": 1.0}),
    ]
}

fn names(apps: &[Value], positions: &[usize]) -> Vec<String> {
    positions
        .iter()
        .map(|&position| apps[position]["display_name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn cpu_descending_lists_the_app_with_no_reading_last() {
    // Goal: an App with no CPU reading sorts after every App with one when the most comes first.
    // Method: order four Apps by CPU, the most first, where one has no CPU reading.
    let apps = apps_by_cpu();
    let order = AppOrder {
        column: AppColumn::Cpu,
        descending: true,
    };

    let positions = app_order(&apps.each_ref(), order, "");

    assert_eq!(names(&apps, &positions), ["a", "b", "d", "c"]);
}

#[test]
fn cpu_ascending_lists_the_app_with_no_reading_last() {
    // Goal: an App with no reading sorts last when the least comes first too, rather than first
    // as a reversed order would put it.
    // Method: order four Apps by CPU, the least first, where one has no CPU reading.
    let apps = apps_by_cpu();
    let order = AppOrder {
        column: AppColumn::Cpu,
        descending: false,
    };

    let positions = app_order(&apps.each_ref(), order, "");

    assert_eq!(names(&apps, &positions), ["d", "b", "a", "c"]);
}

#[test]
fn name_descending_lists_the_names_from_the_end_of_the_alphabet() {
    // Goal: a second click on the App heading turns the name order around.
    // Method: order four Apps by name from Z.
    let apps = apps_by_cpu();
    let order = AppOrder {
        column: AppColumn::Name,
        descending: true,
    };

    let positions = app_order(&apps.each_ref(), order, "");

    assert_eq!(names(&apps, &positions), ["d", "c", "b", "a"]);
}

#[test]
fn search_keeps_the_apps_whose_name_contains_the_text_in_any_case() {
    // Goal: the search ignores the case of the text typed.
    // Method: search four lowercase names for an uppercase letter that only one contains.
    let apps = apps_by_cpu();

    let positions = app_order(&apps.each_ref(), AppOrder::default(), "B");

    assert_eq!(names(&apps, &positions), ["b"]);
}

#[test]
fn search_matches_lowercase_text_in_a_name_with_capitals() {
    // Goal: the search ignores the case of the name as well.
    // Method: search a name with capitals and one without for lowercase text that the first
    // holds only in capitals.
    let apps = [
        json!({"display_name": "Brave"}),
        json!({"display_name": "safari"}),
    ];

    let positions = app_order(&apps.each_ref(), AppOrder::default(), "bra");

    assert_eq!(names(&apps, &positions), ["Brave"]);
}

/// Three Apps whose bundle names end in ".app", which the list leaves out of the names it shows.
fn apps_in_bundles() -> [Value; 3] {
    [
        json!({"display_name": "Foo Bar.app"}),
        json!({"display_name": "AppCleaner.app"}),
        json!({"display_name": "Foo.app"}),
    ]
}

#[test]
fn search_reads_the_name_the_list_shows_without_its_app_extension() {
    // Goal: the search reads the name the list shows, which leaves out ".app".
    // Method: search three bundle names for "app", which only AppCleaner's shown name contains.
    let apps = apps_in_bundles();

    let positions = app_order(&apps.each_ref(), AppOrder::default(), "app");

    assert_eq!(names(&apps, &positions), ["AppCleaner.app"]);
}

#[test]
fn name_ascending_orders_the_names_the_list_shows_without_their_app_extension() {
    // Goal: the name order reads the names the list shows, which leave out ".app".
    // Method: order three bundle names from A, where "Foo" sorts before "Foo Bar" only once
    // their ".app" is left out.
    let apps = apps_in_bundles();
    let order = AppOrder {
        column: AppColumn::Name,
        descending: false,
    };

    let positions = app_order(&apps.each_ref(), order, "");

    assert_eq!(
        names(&apps, &positions),
        ["AppCleaner.app", "Foo.app", "Foo Bar.app"]
    );
}

/// Four Apps whose order differs in each reading column, with a missing reading in most.
fn apps_by_column() -> [Value; 4] {
    [
        AppReadings {
            name: "a",
            members: 1,
            resident: Some(300),
            footprint: Some(100),
            joules: Some(4.0),
            disk_measured: true,
            disk_bps: Some(5.0),
            ..AppReadings::default()
        },
        AppReadings {
            name: "b",
            members: 4,
            footprint: Some(400),
            watts: Some(1.0),
            disk_bps: Some(9.0),
            ..AppReadings::default()
        },
        AppReadings {
            name: "c",
            members: 2,
            resident: Some(900),
            resident_overflowed: true,
            watts: Some(3.0),
            joules: Some(1.0),
            disk_measured: true,
            disk_bps: Some(1.0),
            ..AppReadings::default()
        },
        AppReadings {
            name: "d",
            members: 3,
            resident: Some(100),
            footprint: Some(200),
            watts: Some(2.0),
            joules: Some(2.0),
            disk_measured: true,
            ..AppReadings::default()
        },
    ]
    .map(|readings| readings.app())
}

/// An App's readings: each memory reading is its known bytes, where None means no member was
/// measured, and whether its sum overflowed, and the disk reading is whether any member's bytes
/// were measured and the rate.
#[derive(Default)]
struct AppReadings {
    name: &'static str,
    members: u64,
    resident: Option<u64>,
    resident_overflowed: bool,
    footprint: Option<u64>,
    footprint_overflowed: bool,
    watts: Option<f64>,
    joules: Option<f64>,
    disk_measured: bool,
    disk_bps: Option<f64>,
}

impl AppReadings {
    fn app(&self) -> Value {
        let measure = |known: Option<u64>, overflowed: bool| {
            json!({"known": known, "measured_members": u64::from(known.is_some()),
                "unavailable_members": 0, "overflowed": overflowed})
        };

        json!({"display_name": self.name, "members": self.members,
            "resident_bytes": measure(self.resident, self.resident_overflowed),
            "footprint_bytes": measure(self.footprint, self.footprint_overflowed),
            "energy_watts": self.watts, "energy_joules": self.joules,
            "disk_io_bytes": {"known": 1000, "measured_members": u64::from(self.disk_measured),
                "unavailable_members": 0, "overflowed": false},
            "disk_io_bps": self.disk_bps})
    }
}

macro_rules! column_descending_case {
    ($name:ident, $column:expr, $expected:expr) => {
        #[test]
        fn $name() {
            // Goal: each reading column orders the Apps by its own reading, the most first, with
            // the Apps that have none in it, unmeasured or overflowed, last by name.
            // Method: order four Apps whose order differs in each column by the column.
            let apps = apps_by_column();
            let order = AppOrder {
                column: $column,
                descending: true,
            };

            let positions = app_order(&apps.each_ref(), order, "");

            assert_eq!(names(&apps, &positions), $expected);
        }
    };
}

column_descending_case!(
    members_descending_lists_the_app_with_the_most_members_first,
    AppColumn::Members,
    ["b", "d", "c", "a"]
);
column_descending_case!(
    resident_descending_lists_unmeasured_and_overflowed_memory_last,
    AppColumn::Resident,
    ["a", "d", "b", "c"]
);
column_descending_case!(
    footprint_descending_reads_the_footprint_and_not_the_resident_memory,
    AppColumn::Footprint,
    ["b", "d", "a", "c"]
);
column_descending_case!(
    power_descending_lists_the_app_with_no_power_reading_last,
    AppColumn::Power,
    ["c", "d", "b", "a"]
);
column_descending_case!(
    energy_descending_lists_the_app_with_no_energy_reading_last,
    AppColumn::Energy,
    ["a", "d", "c", "b"]
);
column_descending_case!(
    disk_descending_lists_an_app_whose_disk_bytes_were_unmeasured_last,
    AppColumn::Disk,
    ["a", "c", "b", "d"]
);
