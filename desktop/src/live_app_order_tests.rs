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
    // Order four Apps by CPU, the most first, where one has no CPU reading.
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
    // Order the same Apps by CPU, the least first: no reading still sorts after every reading.
    let apps = apps_by_cpu();
    let order = AppOrder {
        column: AppColumn::Cpu,
        descending: false,
    };

    let positions = app_order(&apps.each_ref(), order, "");

    assert_eq!(names(&apps, &positions), ["d", "b", "a", "c"]);
}

#[test]
fn search_keeps_the_apps_whose_name_contains_the_text_in_any_case() {
    // Search the Apps by an uppercase letter that only one lowercase name contains.
    let apps = apps_by_cpu();

    let positions = app_order(&apps.each_ref(), AppOrder::default(), "B");

    assert_eq!(names(&apps, &positions), ["b"]);
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
    // Search the bundled Apps for "app", which only AppCleaner's shown name contains.
    let apps = apps_in_bundles();

    let positions = app_order(&apps.each_ref(), AppOrder::default(), "app");

    assert_eq!(names(&apps, &positions), ["AppCleaner.app"]);
}

#[test]
fn name_ascending_orders_the_names_the_list_shows_without_their_app_extension() {
    // Order the bundled Apps by name from A, where "Foo" sorts before "Foo Bar" only once their
    // ".app" is left out.
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
        app_reading(
            "a",
            1,
            (Some("300"), false),
            (Some("100"), false),
            None,
            Some(4.0),
            (true, Some(5.0)),
        ),
        app_reading(
            "b",
            4,
            (None, false),
            (Some("400"), false),
            Some(1.0),
            None,
            (false, Some(9.0)),
        ),
        app_reading(
            "c",
            2,
            (Some("900"), true),
            (None, false),
            Some(3.0),
            Some(1.0),
            (true, Some(1.0)),
        ),
        app_reading(
            "d",
            3,
            (Some("100"), false),
            (Some("200"), false),
            Some(2.0),
            Some(2.0),
            (true, None),
        ),
    ]
}

/// An App's readings: each memory reading is its known bytes, where None means no member was
/// measured, and whether the sum overflowed, and its disk reading is whether any member's bytes
/// were measured and the rate.
fn app_reading(
    name: &str,
    members: u64,
    resident: (Option<&str>, bool),
    footprint: (Option<&str>, bool),
    watts: Option<f64>,
    joules: Option<f64>,
    disk: (bool, Option<f64>),
) -> Value {
    let measure = |(known, overflowed): (Option<&str>, bool)| {
        json!({"known": known, "measured_members": u64::from(known.is_some()),
            "unavailable_members": 0, "overflowed": overflowed})
    };
    let (disk_measured, disk_bps) = disk;

    json!({"display_name": name, "members": members,
        "resident_bytes": measure(resident), "footprint_bytes": measure(footprint),
        "energy_watts": watts, "energy_joules": joules,
        "disk_io_bytes": {"known": "1000", "measured_members": u64::from(disk_measured),
            "unavailable_members": 0, "overflowed": false},
        "disk_io_bps": disk_bps})
}

macro_rules! column_descending_case {
    ($name:ident, $column:expr, $expected:expr) => {
        #[test]
        fn $name() {
            // Order the four Apps by the column, the most first, where the Apps with no reading
            // in it, unmeasured or overflowed, sort last by name.
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
