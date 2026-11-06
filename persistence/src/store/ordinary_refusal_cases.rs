//! Independently reported ordinary recovery refusal and retry cases.
use super::*;

#[test]
fn ordinary_metric_omission_and_same_count_numeric_native_forgery_refuse_numeric_omission() {
    // Goal: complete framing/counts cannot replace exact source metric evidence.
    // Method: omit a CPU row, change a numeric value or native bytes, retaining valid schemas.
    ordinary_metric_omission_and_same_count_numeric_native_forgery_refuse(false, "omit");
}

#[test]
fn ordinary_metric_omission_and_same_count_numeric_native_forgery_refuse_numeric_value() {
    // Goal: complete framing/counts cannot replace exact source metric evidence.
    // Method: omit a CPU row, change a numeric value or native bytes, retaining valid schemas.
    ordinary_metric_omission_and_same_count_numeric_native_forgery_refuse(false, "numeric");
}

#[test]
fn ordinary_metric_omission_and_same_count_numeric_native_forgery_refuse_captured_omission() {
    // Goal: complete framing/counts cannot replace exact source metric evidence.
    // Method: omit a CPU row, change a numeric value or native bytes, retaining valid schemas.
    ordinary_metric_omission_and_same_count_numeric_native_forgery_refuse(true, "omit");
}

#[test]
fn ordinary_metric_omission_and_same_count_numeric_native_forgery_refuse_captured_value() {
    // Goal: complete framing/counts cannot replace exact source metric evidence.
    // Method: omit a CPU row, change a numeric value or native bytes, retaining valid schemas.
    ordinary_metric_omission_and_same_count_numeric_native_forgery_refuse(true, "numeric");
}

#[test]
fn ordinary_metric_omission_and_same_count_numeric_native_forgery_refuse_captured_native() {
    // Goal: complete framing/counts cannot replace exact source metric evidence.
    // Method: omit a CPU row, change a numeric value or native bytes, retaining valid schemas.
    ordinary_metric_omission_and_same_count_numeric_native_forgery_refuse(true, "native");
}

#[test]
fn ordinary_equal_content_inode_substitution_after_inventory_refuses_numeric() {
    // Goal: retained census identities survive comparison and normal handoff.
    // Method: replace a present raw table with equal bytes after the live census.
    ordinary_equal_content_inode_substitution_after_inventory_refuses(false);
}

#[test]
fn ordinary_equal_content_inode_substitution_after_inventory_refuses_captured() {
    // Goal: retained census identities survive comparison and normal handoff.
    // Method: replace a present raw table with equal bytes after the live census.
    ordinary_equal_content_inode_substitution_after_inventory_refuses(true);
}

#[test]
fn ordinary_active_boundary_waits_then_same_owner_completes_and_progresses_numeric() {
    // Goal: active same-window silence cannot seal; later-window evidence permits progress.
    // Method: finish metric proof, observe WouldBlock, then publish the next actual ordinary tick.
    ordinary_active_boundary_waits_then_same_owner_completes_and_progresses(false);
}

#[test]
fn ordinary_active_boundary_waits_then_same_owner_completes_and_progresses_captured() {
    // Goal: active same-window silence cannot seal; later-window evidence permits progress.
    // Method: finish metric proof, observe WouldBlock, then publish the next actual ordinary tick.
    ordinary_active_boundary_waits_then_same_owner_completes_and_progresses(true);
}

#[test]
fn ordinary_matching_partial_metadata_repairs_and_contradiction_refuses_numeric_empty() {
    // Goal: only independently reconstructed exact immutable metadata prefixes may be repaired.
    // Method: lose a real proof's metadata suffix, reopen Store, and compare exact full bytes.
    ordinary_matching_partial_metadata_repairs_and_contradiction_refuses(false, 0);
}

#[test]
fn ordinary_matching_partial_metadata_repairs_and_contradiction_refuses_numeric_prefix() {
    // Goal: only independently reconstructed exact immutable metadata prefixes may be repaired.
    // Method: lose a real proof's metadata suffix, reopen Store, and compare exact full bytes.
    ordinary_matching_partial_metadata_repairs_and_contradiction_refuses(false, 137);
}

#[test]
fn ordinary_matching_partial_metadata_repairs_and_contradiction_refuses_numeric_prefix_complete() {
    // Goal: only independently reconstructed exact immutable metadata prefixes may be repaired.
    // Method: lose a real proof's metadata suffix, reopen Store, and compare exact full bytes.
    ordinary_matching_partial_metadata_repairs_and_contradiction_refuses(false, 512);
}

#[test]
fn ordinary_matching_partial_metadata_repairs_and_contradiction_refuses_numeric_inner() {
    // Goal: only independently reconstructed exact immutable metadata prefixes may be repaired.
    // Method: lose a real proof's metadata suffix, reopen Store, and compare exact full bytes.
    ordinary_matching_partial_metadata_repairs_and_contradiction_refuses(false, 700);
}

#[test]
fn ordinary_matching_partial_metadata_repairs_and_contradiction_refuses_numeric_contradiction() {
    // Goal: only independently reconstructed exact immutable metadata prefixes may be repaired.
    // Method: lose a real proof's metadata suffix, reopen Store, and compare exact full bytes.
    ordinary_matching_partial_metadata_repairs_and_contradiction_refuses(false, usize::MAX);
}

#[test]
fn ordinary_matching_partial_metadata_repairs_and_contradiction_refuses_captured_empty() {
    // Goal: only independently reconstructed exact immutable metadata prefixes may be repaired.
    // Method: lose a real proof's metadata suffix, reopen Store, and compare exact full bytes.
    ordinary_matching_partial_metadata_repairs_and_contradiction_refuses(true, 0);
}

#[test]
fn ordinary_matching_partial_metadata_repairs_and_contradiction_refuses_captured_prefix() {
    // Goal: only independently reconstructed exact immutable metadata prefixes may be repaired.
    // Method: lose a real proof's metadata suffix, reopen Store, and compare exact full bytes.
    ordinary_matching_partial_metadata_repairs_and_contradiction_refuses(true, 137);
}

#[test]
fn ordinary_matching_partial_metadata_repairs_and_contradiction_refuses_captured_prefix_complete() {
    // Goal: only independently reconstructed exact immutable metadata prefixes may be repaired.
    // Method: lose a real proof's metadata suffix, reopen Store, and compare exact full bytes.
    ordinary_matching_partial_metadata_repairs_and_contradiction_refuses(true, 512);
}

#[test]
fn ordinary_matching_partial_metadata_repairs_and_contradiction_refuses_captured_inner() {
    // Goal: only independently reconstructed exact immutable metadata prefixes may be repaired.
    // Method: lose a real proof's metadata suffix, reopen Store, and compare exact full bytes.
    ordinary_matching_partial_metadata_repairs_and_contradiction_refuses(true, 700);
}

#[test]
fn ordinary_matching_partial_metadata_repairs_and_contradiction_refuses_captured_contradiction() {
    // Goal: only independently reconstructed exact immutable metadata prefixes may be repaired.
    // Method: lose a real proof's metadata suffix, reopen Store, and compare exact full bytes.
    ordinary_matching_partial_metadata_repairs_and_contradiction_refuses(true, usize::MAX);
}

#[test]
fn ordinary_shared_budget_refusal_and_source_substitution_keep_authority_numeric() {
    // Goal: failed construction and a different source grant no raw or publication authority.
    // Method: exhaust the shared budget, then offer an independently bound source object.
    ordinary_shared_budget_refusal_and_source_substitution_keep_authority(false);
}

#[test]
fn ordinary_shared_budget_refusal_and_source_substitution_keep_authority_captured() {
    // Goal: failed construction and a different source grant no raw or publication authority.
    // Method: exhaust the shared budget, then offer an independently bound source object.
    ordinary_shared_budget_refusal_and_source_substitution_keep_authority(true);
}

#[test]
fn ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material_numeric_missing() {
    // Goal: missing/torn metrics remain occupied, and schema-valid clock/name forgery refuses.
    // Method: alter one complete payload fact before its first identity admission.
    ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material(false, "missing");
}

#[test]
fn ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material_numeric_torn() {
    // Goal: missing/torn metrics remain occupied, and schema-valid clock/name forgery refuses.
    // Method: alter one complete payload fact before its first identity admission.
    ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material(false, "torn");
}

#[test]
fn ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material_numeric_extra() {
    // Goal: missing/torn metrics remain occupied, and schema-valid clock/name forgery refuses.
    // Method: alter one complete payload fact before its first identity admission.
    ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material(false, "extra");
}

#[test]
fn ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material_numeric_clock() {
    // Goal: missing/torn metrics remain occupied, and schema-valid clock/name forgery refuses.
    // Method: alter one complete payload fact before its first identity admission.
    ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material(false, "clock");
}

#[test]
fn ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material_numeric_half_group() {
    // Goal: missing/torn metrics remain occupied, and schema-valid clock/name forgery refuses.
    // Method: alter one complete payload fact before its first identity admission.
    ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material(false, "half-group");
}

#[test]
fn ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material_captured_missing() {
    // Goal: missing/torn metrics remain occupied, and schema-valid clock/name forgery refuses.
    // Method: alter one complete payload fact before its first identity admission.
    ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material(true, "missing");
}

#[test]
fn ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material_captured_torn() {
    // Goal: missing/torn metrics remain occupied, and schema-valid clock/name forgery refuses.
    // Method: alter one complete payload fact before its first identity admission.
    ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material(true, "torn");
}

#[test]
fn ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material_captured_extra() {
    // Goal: missing/torn metrics remain occupied, and schema-valid clock/name forgery refuses.
    // Method: alter one complete payload fact before its first identity admission.
    ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material(true, "extra");
}

#[test]
fn ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material_captured_clock() {
    // Goal: missing/torn metrics remain occupied, and schema-valid clock/name forgery refuses.
    // Method: alter one complete payload fact before its first identity admission.
    ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material(true, "clock");
}

#[test]
fn ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material_captured_half_group() {
    // Goal: missing/torn metrics remain occupied, and schema-valid clock/name forgery refuses.
    // Method: alter one complete payload fact before its first identity admission.
    ordinary_missing_torn_extra_names_and_forged_clocks_preserve_material(true, "half-group");
}

#[test]
fn ordinary_missing_source_before_proof_does_not_become_loss_numeric() {
    // Goal: source absence supplies no metric completeness or loss/closure evidence.
    // Method: remove only the exact owned transport before reacquiring missing metadata.
    ordinary_missing_source_before_proof_does_not_become_loss(false);
}

#[test]
fn ordinary_missing_source_before_proof_does_not_become_loss_captured() {
    // Goal: source absence supplies no metric completeness or loss/closure evidence.
    // Method: remove only the exact owned transport before reacquiring missing metadata.
    ordinary_missing_source_before_proof_does_not_become_loss(true);
}

#[test]
fn ordinary_reconstruction_never_undoes_selected_salvage_census_numeric_kind_1() {
    // Goal: once a census consumes normal authority, reconstruction cannot reacquire it.
    // Method: stop at both actual committed census states and try ordinary entrypoints.
    ordinary_reconstruction_never_undoes_selected_salvage_census(false, 1);
}

#[test]
fn ordinary_reconstruction_never_undoes_selected_salvage_census_numeric_kind_2() {
    // Goal: once a census consumes normal authority, reconstruction cannot reacquire it.
    // Method: stop at both actual committed census states and try ordinary entrypoints.
    ordinary_reconstruction_never_undoes_selected_salvage_census(false, 2);
}

#[test]
fn ordinary_reconstruction_never_undoes_selected_salvage_census_captured_kind_1() {
    // Goal: once a census consumes normal authority, reconstruction cannot reacquire it.
    // Method: stop at both actual committed census states and try ordinary entrypoints.
    ordinary_reconstruction_never_undoes_selected_salvage_census(true, 1);
}

#[test]
fn ordinary_reconstruction_never_undoes_selected_salvage_census_captured_kind_2() {
    // Goal: once a census consumes normal authority, reconstruction cannot reacquire it.
    // Method: stop at both actual committed census states and try ordinary entrypoints.
    ordinary_reconstruction_never_undoes_selected_salvage_census(true, 2);
}

#[test]
fn ordinary_actual_header_change_refuses_then_same_owner_retries_numeric_word_2() {
    // Goal: the registered immutable source fields are reobserved after construction.
    // Method: mutate the owned header through the existing fault fixture, then restore it.
    ordinary_actual_header_change_refuses_then_same_owner_retries(false, 2);
}

#[test]
fn ordinary_actual_header_change_refuses_then_same_owner_retries_numeric_word_3() {
    // Goal: the registered immutable source fields are reobserved after construction.
    // Method: mutate the owned header through the existing fault fixture, then restore it.
    ordinary_actual_header_change_refuses_then_same_owner_retries(false, 3);
}

#[test]
fn ordinary_actual_header_change_refuses_then_same_owner_retries_numeric_word_6() {
    // Goal: the registered immutable source fields are reobserved after construction.
    // Method: mutate the owned header through the existing fault fixture, then restore it.
    ordinary_actual_header_change_refuses_then_same_owner_retries(false, 6);
}

#[test]
fn ordinary_actual_header_change_refuses_then_same_owner_retries_numeric_word_40() {
    // Goal: the registered immutable source fields are reobserved after construction.
    // Method: mutate the owned header through the existing fault fixture, then restore it.
    ordinary_actual_header_change_refuses_then_same_owner_retries(false, 40);
}

#[test]
fn ordinary_actual_header_change_refuses_then_same_owner_retries_numeric_word_43() {
    // Goal: the registered immutable source fields are reobserved after construction.
    // Method: mutate the owned header through the existing fault fixture, then restore it.
    ordinary_actual_header_change_refuses_then_same_owner_retries(false, 43);
}

#[test]
fn ordinary_actual_header_change_refuses_then_same_owner_retries_captured_word_2() {
    // Goal: the registered immutable source fields are reobserved after construction.
    // Method: mutate the owned header through the existing fault fixture, then restore it.
    ordinary_actual_header_change_refuses_then_same_owner_retries(true, 2);
}

#[test]
fn ordinary_actual_header_change_refuses_then_same_owner_retries_captured_word_3() {
    // Goal: the registered immutable source fields are reobserved after construction.
    // Method: mutate the owned header through the existing fault fixture, then restore it.
    ordinary_actual_header_change_refuses_then_same_owner_retries(true, 3);
}

#[test]
fn ordinary_actual_header_change_refuses_then_same_owner_retries_captured_word_6() {
    // Goal: the registered immutable source fields are reobserved after construction.
    // Method: mutate the owned header through the existing fault fixture, then restore it.
    ordinary_actual_header_change_refuses_then_same_owner_retries(true, 6);
}

#[test]
fn ordinary_actual_header_change_refuses_then_same_owner_retries_captured_word_40() {
    // Goal: the registered immutable source fields are reobserved after construction.
    // Method: mutate the owned header through the existing fault fixture, then restore it.
    ordinary_actual_header_change_refuses_then_same_owner_retries(true, 40);
}

#[test]
fn ordinary_actual_header_change_refuses_then_same_owner_retries_captured_word_43() {
    // Goal: the registered immutable source fields are reobserved after construction.
    // Method: mutate the owned header through the existing fault fixture, then restore it.
    ordinary_actual_header_change_refuses_then_same_owner_retries(true, 43);
}
