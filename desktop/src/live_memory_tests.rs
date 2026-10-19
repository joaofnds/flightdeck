use serde_json::{Value, json};

use super::{MemoryQuantity, quantities};

fn selection() -> Value {
    let identity = json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004", "publication":9,
        "wall_ns":1_000_000_000_000_u64});
    let mut summary = identity.clone();
    summary["memory_total_bytes"] = json!(51_539_607_552_u64);
    summary["memory_active_bytes"] = json!(1_073_741_824_u64);
    summary["memory_wired_bytes"] = json!(536_870_912_u64);
    summary["memory_compressed_bytes"] = json!(0);
    summary["memory_free_bytes"] = json!(2_147_483_648_u64);
    summary["swap_used_bytes"] = Value::Null;
    summary["pressure"] = json!(1);
    json!({"selected":identity,"summary":summary})
}

#[test]
fn exposes_all_selected_quantities_with_independent_missingness() {
    // Goal: the selected observation supplies every VM quantity without inventing totals.
    // Method: compare all thirteen readouts for measured, zero and unavailable fields.
    let rows = quantities(&selection());

    assert_eq!(
        rows,
        vec![
            row("Installed", "48 GiB"),
            row("Active", "1 GiB"),
            row("Wired", "512 MiB"),
            row("Compressed", "0 B"),
            row("Free", "2 GiB"),
            row("Inactive", "Unavailable"),
            row("Speculative", "Unavailable"),
            row("Anonymous", "Unavailable"),
            row("File-backed", "Unavailable"),
            row("Purgeable", "Unavailable"),
            row("Swap used", "Unavailable"),
            row("Swap allocated", "Unavailable"),
            row("Memory pressure", "Warning"),
        ]
    );
}

#[test]
fn measured_quantities_keep_each_vm_and_swap_field_distinct() {
    // Goal: each named quantity reads its own measured field, including less common gauges.
    // Method: give all twelve byte fields distinct GiB values and compare every readout.
    let mut input = selection();
    input["summary"]["memory_total_bytes"] = json!(1_073_741_824_u64);
    input["summary"]["memory_active_bytes"] = json!(2_147_483_648_u64);
    input["summary"]["memory_wired_bytes"] = json!(3_221_225_472_u64);
    input["summary"]["memory_compressed_bytes"] = json!(4_294_967_296_u64);
    input["summary"]["memory_free_bytes"] = json!(5_368_709_120_u64);
    input["summary"]["memory_inactive_bytes"] = json!(6_442_450_944_u64);
    input["summary"]["memory_speculative_bytes"] = json!(7_516_192_768_u64);
    input["summary"]["memory_anonymous_bytes"] = json!(8_589_934_592_u64);
    input["summary"]["memory_file_backed_bytes"] = json!(9_663_676_416_u64);
    input["summary"]["memory_purgeable_bytes"] = json!(10_737_418_240_u64);
    input["summary"]["swap_used_bytes"] = json!(11_811_160_064_u64);
    input["summary"]["swap_total_bytes"] = json!(12_884_901_888_u64);

    let rows = quantities(&input);

    assert_eq!(
        rows,
        vec![
            row("Installed", "1 GiB"),
            row("Active", "2 GiB"),
            row("Wired", "3 GiB"),
            row("Compressed", "4 GiB"),
            row("Free", "5 GiB"),
            row("Inactive", "6 GiB"),
            row("Speculative", "7 GiB"),
            row("Anonymous", "8 GiB"),
            row("File-backed", "9 GiB"),
            row("Purgeable", "10 GiB"),
            row("Swap used", "11 GiB"),
            row("Swap allocated", "12 GiB"),
            row("Memory pressure", "Warning"),
        ]
    );
}

fn row(name: &'static str, reading: &str) -> MemoryQuantity {
    MemoryQuantity {
        name,
        reading: reading.to_owned(),
    }
}

#[test]
fn refuses_a_summary_from_another_same_second_publication() {
    // Goal: a sibling observation cannot lend quantities to the selected identity.
    // Method: change only the summary publication while retaining its measured gauges.
    let mut selection = selection();
    selection["summary"]["publication"] = json!(10);

    let rows = quantities(&selection);

    assert_eq!(rows.len(), 13);
    assert!(rows.iter().all(|row| row.reading == "Unavailable"));
}

#[test]
fn missing_selection_does_not_borrow_the_latest_summary() {
    // Goal: a gap shows unavailable readouts even when an unrelated summary is supplied.
    // Method: remove the selected observation while leaving all summary quantities intact.
    let mut selection = selection();
    selection["selected"] = Value::Null;

    let rows = quantities(&selection);

    assert_eq!(rows.len(), 13);
    assert!(rows.iter().all(|row| row.reading == "Unavailable"));
}

#[test]
fn unknown_pressure_remains_unavailable() {
    // Goal: future or invalid pressure codes never become a recognized macOS state.
    // Method: retain a matching identity with a pressure code outside the known domain.
    let mut selection = selection();
    selection["summary"]["pressure"] = json!(9);

    let rows = quantities(&selection);

    assert_eq!(rows.last(), Some(&row("Memory pressure", "Unavailable")));
}
