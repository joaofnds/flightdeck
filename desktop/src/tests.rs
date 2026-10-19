use crate::charts::{Inspection, domain};
use crate::data::{Capture, Catalog, Panel};

#[test]
fn keeps_captured_missing_samples_unavailable() {
    // Decode the actual bundled observations and check that absent readings survive.
    let capture = Capture::load().expect("valid captured observations");

    let missing = capture
        .series
        .values()
        .flatten()
        .flat_map(|row| &row.values)
        .filter(|value| value.is_none())
        .count();

    assert!(missing > 0);
    assert_eq!(capture.value("appCpu", 0), None);
    assert_eq!(
        crate::data::reading(capture.value("appCpu", 0), "%"),
        "Unavailable"
    );
    assert_eq!(capture.value("no-such-metric", 120), None);
}

#[test]
fn loads_every_captured_destination_and_both_ranges() {
    // Exercise the complete bundled schema rather than a reduced hand-built fixture.
    let catalog = Catalog::load().expect("valid captured panel catalog");

    assert_eq!(catalog.navigation.len(), 11);
    assert_eq!(catalog.pages.len(), 11);
    assert_eq!(catalog.hour_pages.len(), 11);
    assert_eq!(catalog.app_pages.len(), 12);
    assert_eq!(catalog.self_panels.len(), 6);
    let routes: std::collections::BTreeSet<_> =
        catalog.navigation.iter().map(|entry| &entry[0]).collect();
    assert_eq!(
        catalog
            .pages
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        routes
    );
    assert_eq!(
        catalog
            .hour_pages
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        routes
    );
}

#[test]
fn keeps_installed_memory_independent_of_stacked_categories() {
    // Installed RAM is a reference, so it must not be added to the memory stack.
    let panel: Panel = serde_json::from_value(serde_json::json!({
        "id":"memory", "title":"Memory", "kind":"chart", "unit":"GiB", "stack":true,
        "lines":[
            {"name":"App", "color":"#5794f2", "values":[12,14]},
            {"name":"Wired", "color":"#ff9830", "values":[4,6]},
            {"name":"Installed", "color":"#ccccdc", "stack":false, "values":[48,48]}
        ]
    }))
    .expect("valid chart fixture");
    let inspection = Inspection {
        lower: 0,
        selected: 1,
        upper: 1,
    };

    let (_, maximum) = domain(&panel, inspection, &[]);

    // The axis keeps 4% headroom over installed RAM and ticks at 16 GiB, as the prototype does.
    assert!((maximum - 49.92).abs() < 0.001);
}

#[test]
fn gives_outgoing_traffic_room_below_zero() {
    // Null sends remain absent, while a real send expands the signed vertical scale.
    let panel: Panel = serde_json::from_value(serde_json::json!({
        "id":"network", "title":"Network", "kind":"chart", "unit":"MB/s",
        "lines":[
            {"name":"Receive", "color":"#5794f2", "values":[2,3]},
            {"name":"Send", "color":"#73bf69", "negative":true, "values":[null,8]}
        ]
    }))
    .expect("valid chart fixture");

    let (minimum, maximum) = domain(
        &panel,
        Inspection {
            lower: 0,
            selected: 1,
            upper: 1,
        },
        &[],
    );

    // The prototype rounds the padded range out to its 2.5 MB/s ticks.
    assert!((minimum + 10.0).abs() < f32::EPSILON);
    assert!((maximum - 5.0).abs() < f32::EPSILON);
}

#[test]
fn clamps_inspection_before_the_first_cell() {
    // Pointer positions outside the plotting area select the nearest captured cell.
    let inspection = Inspection {
        lower: 90,
        selected: 120,
        upper: 120,
    };

    assert_eq!(inspection.select_fraction(-1.0), 90);
}

#[test]
fn clamps_inspection_after_the_last_cell() {
    // The right border belongs to the last sample, not a nonexistent sample 121.
    let inspection = Inspection {
        lower: 90,
        selected: 120,
        upper: 120,
    };

    assert_eq!(inspection.select_fraction(1.0), 120);
}

#[test]
fn retains_app_totals_when_process_histories_were_not_captured() {
    // A ranking link for grafana must still lead to its six app-total histories.
    let catalog = Catalog::load().expect("valid captured catalog");
    let capture = Capture::load().expect("valid captured observations");

    let panels = catalog.app_panels(&capture, "grafana");

    assert_eq!(panels.len(), 6);
    assert!(panels.iter().all(|panel| panel.lines[0].name == "grafana"));
    let memory = panels
        .iter()
        .find(|panel| panel.id == "selectedappMemory")
        .expect("memory history");
    assert!(memory.lines[0].values[120].is_some_and(|value| value > 0.0));
}

#[test]
fn keeps_secondary_units_out_of_the_primary_scale() {
    // Real open-file counts and their percentage of a limit have separate domains.
    let catalog = Catalog::load().expect("valid captured catalog");
    let panel = catalog.pages["cpu"]
        .iter()
        .find(|panel| panel.id == "openFiles")
        .expect("open files chart");
    let inspection = Inspection {
        lower: 90,
        selected: 120,
        upper: 120,
    };

    let (_, count_maximum) = domain(panel, inspection, &[]);
    let (_, share_maximum) = crate::charts::axis_domain(panel, inspection, &[], "right1");

    assert!(count_maximum > 10_000.0);
    assert!(share_maximum < 100.0);
    assert_eq!(panel.lines[1].unit.as_deref(), Some("%"));
}
