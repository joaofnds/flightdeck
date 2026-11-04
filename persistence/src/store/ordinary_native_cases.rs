//! Independently reported native ordinary recovery interruption cases.
use super::*;

#[test]
fn ordinary_native_numeric_open_batch_retry() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "open-batch", "retry");
}

#[test]
fn ordinary_native_numeric_read_table_retry() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "read-table", "retry");
}

#[test]
fn ordinary_native_numeric_read_index_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "read-index", "process");
}

#[test]
fn ordinary_native_numeric_create_store() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "create", "store");
}

#[test]
fn ordinary_native_numeric_prefix_retry() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "prefix", "retry");
}

#[test]
fn ordinary_native_numeric_inner_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "inner", "process");
}

#[test]
fn ordinary_native_numeric_complete_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "complete", "process");
}

#[test]
fn ordinary_native_numeric_metadata_sync_store() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "metadata-sync", "store");
}

#[test]
fn ordinary_native_numeric_ready_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "ready", "process");
}

#[test]
fn ordinary_native_numeric_rename_retry() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "rename", "retry");
}

#[test]
fn ordinary_native_numeric_ready_directory_store() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "ready-directory", "store");
}

#[test]
fn ordinary_native_numeric_busy_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "busy", "process");
}

#[test]
fn ordinary_native_numeric_gate_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "gate", "process");
}

#[test]
fn ordinary_native_numeric_quiet_store() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "quiet", "store");
}

#[test]
fn ordinary_native_numeric_idle_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(false, "idle", "process");
}

#[test]
fn ordinary_native_captured_constructor_open_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "constructor-open", "process");
}

#[test]
fn ordinary_native_captured_open_batch_retry() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "open-batch", "retry");
}

#[test]
fn ordinary_native_captured_read_table_retry() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "read-table", "retry");
}

#[test]
fn ordinary_native_captured_read_index_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "read-index", "process");
}

#[test]
fn ordinary_native_captured_create_store() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "create", "store");
}

#[test]
fn ordinary_native_captured_prefix_retry() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "prefix", "retry");
}

#[test]
fn ordinary_native_captured_inner_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "inner", "process");
}

#[test]
fn ordinary_native_captured_complete_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "complete", "process");
}

#[test]
fn ordinary_native_captured_metadata_sync_store() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "metadata-sync", "store");
}

#[test]
fn ordinary_native_captured_ready_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "ready", "process");
}

#[test]
fn ordinary_native_captured_rename_retry() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "rename", "retry");
}

#[test]
fn ordinary_native_captured_ready_directory_store() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "ready-directory", "store");
}

#[test]
fn ordinary_native_captured_busy_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "busy", "process");
}

#[test]
fn ordinary_native_captured_gate_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "gate", "process");
}

#[test]
fn ordinary_native_captured_quiet_store() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "quiet", "store");
}

#[test]
fn ordinary_native_captured_idle_process() {
    // Goal: one interrupted ordinary transaction retains its rows and recovery authority.
    // Method: inject the named native fault, then retry or reacquire as this case specifies.
    native_case_run(true, "idle", "process");
}
