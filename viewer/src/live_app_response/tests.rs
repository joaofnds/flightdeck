use super::*;

#[test]
fn preserves_known_subtotals_and_uncertain_membership() {
    // Goal: presentation owns exact known quantities without claiming complete membership.
    // Method: serialize the specified two-process App and compare independent constants.
    let sample = fixture::sample(fixture::Scenario::Ordinary);

    let page = sample
        .apps_page(Page {
            offset: 0,
            limit: 512,
        })
        .unwrap();

    assert_eq!(page["apps"][0]["cpu_ns"]["known"], "500000000");
    assert_eq!(page["apps"][0]["cpu_percent_one_core"], 50.0);
    assert_eq!(page["apps"][0]["energy_nj"]["known"], "3000000000");
    assert_eq!(page["apps"][0]["energy_joules"], 3.0);
    assert_eq!(page["apps"][0]["energy_watts"], 3.0);
    assert_eq!(page["apps"][0]["resident_bytes"]["known"], "157286400");
    assert_eq!(page["apps"][0]["membership_complete"], false);
    assert_eq!(page["selected"]["stamp"]["publication"], "9007199254740993");
}

pub(super) mod fixture;

#[test]
fn keeps_partial_disk_rates_and_joint_coverage_on_the_wire() {
    // Goal: a refused process cannot erase measured I/O or become a measured zero.
    // Method: serialize the known member's read/write/combined amounts beside one refusal.
    let sample = fixture::sample(fixture::Scenario::Refused);

    let detail = sample
        .app_detail(&Detail {
            selected: &sample.selected,
            app_index: 0,
            page: Page {
                offset: 0,
                limit: 512,
            },
        })
        .unwrap();

    assert_eq!(
        detail["app"]["disk_io_bytes"],
        serde_json::json!({
            "known":"1200000000", "measured_members":1,
            "unavailable_members":1, "overflowed":false
        })
    );
    assert_eq!(detail["app"]["disk_read_bps"], 200_000_000.0);
    assert_eq!(detail["app"]["disk_written_bps"], 1_000_000_000.0);
    assert_eq!(detail["app"]["disk_io_bps"], 1_200_000_000.0);
    assert_eq!(
        detail["processes"][1]["disk_io_bytes"],
        serde_json::json!({
            "known":null, "unavailable":"refused"
        })
    );
    assert_eq!(detail["processes"][1]["disk_io_bps"], Value::Null);
}

#[test]
fn preserves_process_identity_and_exact_byte_keys_in_detail() {
    // Goal: detail cannot lose creation precision or identify an App by its display text.
    // Method: compare one selected process and its enclosing App against fixed wire literals.
    let sample = fixture::sample(fixture::Scenario::Ordinary);

    let detail = sample
        .app_detail(&Detail {
            selected: &sample.selected,
            app_index: 0,
            page: Page {
                offset: 0,
                limit: 512,
            },
        })
        .unwrap();

    assert_eq!(
        detail["app"]["key"],
        serde_json::json!({
            "kind": "bundle", "bytes_hex": "2f412e617070", "display": "/A.app"
        })
    );
    assert_eq!(
        detail["processes"][0]["creation"],
        serde_json::json!({
            "unique_id": "9007199254740993", "pid_version": 1
        })
    );
    assert_eq!(
        detail["processes"][0]["cpu_ns"],
        serde_json::json!({
            "known": "200000000", "unavailable": null
        })
    );
    assert_eq!(detail["processes"][0]["state"], "live");
    assert_eq!(detail["selected"]["wall_ns"], "1800000000000000000");
}

#[test]
fn distinguishes_unavailable_members_from_measured_zero() {
    // Goal: a refused member never becomes measured zero in an App or a process.
    // Method: refuse one of two members and inspect subtotal coverage and its quantities.
    let sample = fixture::sample(fixture::Scenario::Refused);

    let detail = sample
        .app_detail(&Detail {
            selected: &sample.selected,
            app_index: 0,
            page: Page {
                offset: 0,
                limit: 512,
            },
        })
        .unwrap();

    assert_eq!(
        detail["app"]["cpu_ns"],
        serde_json::json!({ "known": "200000000",
        "measured_members": 1, "unavailable_members": 1, "overflowed": false })
    );
    assert_eq!(
        detail["processes"][1]["cpu_ns"],
        serde_json::json!({
            "known": null, "unavailable": "refused"
        })
    );
    assert_eq!(
        detail["processes"][1]["resident_bytes"],
        serde_json::json!({
            "known": null, "unavailable": "unknown"
        })
    );
    assert_eq!(detail["processes"][1]["cpu_percent_one_core"], Value::Null);
}

macro_rules! missing_rates {
    ($name:ident, $scenario:expr, $reason:literal) => {
        #[test]
        fn $name() {
            // Goal: unsupported rate intervals keep selected gauges without inventing deltas.
            // Method: inspect an independently named source-boundary scenario.
            let sample = fixture::sample($scenario);

            let detail = sample.app_detail(&Detail { selected: &sample.selected, app_index: 0,
                page: Page { offset: 0, limit: 512 } }).unwrap();

            assert_eq!(detail["processes"][0]["cpu_ns"], serde_json::json!({
                "known": null, "unavailable": $reason
            }));
            assert_eq!(detail["app"]["cpu_percent_one_core"], Value::Null);
            assert_eq!(detail["app"]["resident_bytes"]["known"], "157286400");
            assert_eq!(detail["app"]["membership_complete"], false);
            assert_eq!(detail["processes"][0]["disk_read_bytes"], serde_json::json!({
                "known":null, "unavailable":$reason
            }));
            assert_eq!(detail["processes"][0]["disk_written_bps"], Value::Null);
            assert_eq!(detail["app"]["disk_io_bps"], Value::Null);
        }
    }
}
missing_rates!(
    keeps_first_seen_creations_without_a_zero_baseline,
    fixture::Scenario::Birth,
    "baseline"
);
missing_rates!(
    keeps_publication_gaps_without_a_rate,
    fixture::Scenario::Gap,
    "interval"
);

#[test]
fn retains_process_usage_but_withholds_app_attribution_across_exec() {
    // Goal: an exec interval remains a process measurement without an invented App split.
    // Method: change PID version between two otherwise compatible observations.
    let sample = fixture::sample(fixture::Scenario::Exec);

    let detail = sample
        .app_detail(&Detail {
            selected: &sample.selected,
            app_index: 0,
            page: Page {
                offset: 0,
                limit: 512,
            },
        })
        .unwrap();

    assert_eq!(detail["processes"][0]["cpu_ns"]["known"], "200000000");
    assert_eq!(detail["processes"][0]["app_attribution"], "exec");
    assert_eq!(detail["app"]["cpu_ns"]["measured_members"], 0);
    assert_eq!(detail["app"]["cpu_percent_one_core"], Value::Null);
    assert_eq!(
        detail["processes"][0]["disk_io_bytes"]["known"],
        "1200000000"
    );
    assert_eq!(detail["processes"][0]["disk_io_bps"], 1_200_000_000.0);
    assert_eq!(detail["app"]["disk_io_bytes"]["measured_members"], 0);
    assert_eq!(detail["app"]["disk_io_bps"], Value::Null);
}

#[test]
fn retains_legacy_gauges_with_unsupported_identity() {
    // Goal: unsupported identity cannot manufacture App groups or exact process rates.
    // Method: serialize two observations with numeric usage but legacy identity absence.
    let sample = fixture::sample(fixture::Scenario::Legacy);

    let page = sample
        .ungrouped_page(Page {
            offset: 0,
            limit: 512,
        })
        .unwrap();

    assert_eq!(page["app_count"], 0);
    assert_eq!(page["ungrouped_members"], 2);
    assert_eq!(page["processes"][0]["program_unavailable"], "unsupported");
    assert_eq!(page["processes"][0]["resident_bytes"]["known"], "104857600");
    assert_eq!(page["processes"][0]["creation"], Value::Null);
}

#[test]
fn keeps_invalid_bytes_separate_from_literal_escape_text() {
    // Goal: arbitrary captured bytes stay reversible and distinct from escaped display text.
    // Method: serialize an invalid UTF-8 name and a literal backslash escape name.
    let sample = fixture::sample(fixture::Scenario::Bytes);

    let page = sample
        .apps_page(Page {
            offset: 0,
            limit: 512,
        })
        .unwrap();

    assert_eq!(
        page["apps"][0]["key"],
        serde_json::json!({ "kind": "executable",
        "bytes_hex": "6e616d655c786666", "display": "name\\\\xff" })
    );
    assert_eq!(
        page["apps"][1]["key"],
        serde_json::json!({ "kind": "executable",
        "bytes_hex": "6e616d65ff", "display": "name\\xff" })
    );
}

#[test]
fn retains_exact_overflow_subtotals_without_derived_rates() {
    // Goal: a subtotal overflow cannot present a plausible numeric rate or lose exact integers.
    // Method: overflow the second addition after a measured u64 maximum.
    let sample = fixture::sample(fixture::Scenario::Overflow);

    let page = sample
        .apps_page(Page {
            offset: 0,
            limit: 512,
        })
        .unwrap();

    assert_eq!(
        page["apps"][0]["cpu_ns"],
        serde_json::json!({
            "known": "18446744073709551615", "measured_members": 1,
            "unavailable_members": 1, "overflowed": true
        })
    );
    assert_eq!(page["apps"][0]["cpu_percent_one_core"], Value::Null);
    assert_eq!(page["apps"][0]["energy_joules"], Value::Null);
    assert_eq!(page["apps"][0]["energy_watts"], Value::Null);
}

#[test]
fn rejects_an_app_index_from_a_different_observation() {
    // Goal: sample-relative indices cannot cross a publication boundary.
    // Method: ask for a valid App index with a different selected publication identity.
    let sample = fixture::sample(fixture::Scenario::Ordinary);
    let mut selected = sample.selected;
    selected.stamp.publication += 1;

    let result = sample.app_detail(&Detail {
        selected: &selected,
        app_index: 0,
        page: Page {
            offset: 0,
            limit: 512,
        },
    });

    assert!(matches!(result, Err(Error::Bound("live App stale detail"))));
}

#[test]
fn names_the_app_from_its_captured_outer_bundle_component() {
    // Goal: readable App naming uses captured bytes while the full path remains its identity.
    // Method: compare the displayed component and separately retained path of a captured bundle.
    let sample = fixture::sample(fixture::Scenario::Ordinary);

    let page = sample
        .apps_page(Page {
            offset: 0,
            limit: 512,
        })
        .unwrap();

    assert_eq!(page["apps"][0]["display_name"], "A.app");
    assert_eq!(page["apps"][0]["key"]["display"], "/A.app");
}
