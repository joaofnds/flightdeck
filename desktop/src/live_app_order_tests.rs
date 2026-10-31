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
