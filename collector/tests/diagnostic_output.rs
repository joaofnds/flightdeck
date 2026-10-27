#[cfg(test)]
#[path = "diagnostic_output/mod.rs"]
mod diagnostic_output;

use diagnostic_output::{Harness, Output};

const FILLER_ROLE: &str = "FLIGHTDECK_DIAGNOSTIC_FILLER";
const TEST: &str = "finishes_recording_with_a_permanently_full_stdout";

#[test]
fn finishes_recording_with_a_permanently_full_stdout() {
    // Goal: a permanently full stdout cannot stop startup, tick ten or finite persistence.
    // Method: an external controller prefills output in a separate child, then owns all waits.
    if std::env::var_os(FILLER_ROLE).is_some() {
        diagnostic_output::fill_stdout();
    }
    let mut harness = Harness::new(TEST);
    let sink = harness.full_sink(TEST, FILLER_ROLE);

    harness.start(Output::Supplied(sink));
    let observed = harness.observe();
    harness.cleanup().unwrap();

    assert!(
        observed.status.is_some_and(|status| status.success()),
        "collector failed: {observed:?}"
    );
    assert!(observed.publications >= 12, "{observed:?}");
    assert!(observed.completed, "{observed:?}");
    assert!(observed.sealed, "{observed:?}");
    assert!(
        observed.persister_count > 0,
        "no actual persistence child observed"
    );
    assert!(
        observed.persister_absent,
        "persistence child survived: {observed:?}"
    );
    harness.remove_successful_fixture();
}

#[test]
fn finishes_recording_with_a_broken_stdout() {
    // Goal: a failed stdout cannot stop actual collection or its persistence child.
    // Method: run through tick twelve with the private socket's reader already closed.
    let mut harness = Harness::new("broken_stdout");

    harness.start_with_stderr(
        Output::Supplied(Harness::broken_sink()),
        Harness::broken_sink(),
    );
    let observed = harness.observe();
    harness.cleanup().unwrap();

    assert!(
        observed.status.is_some_and(|status| status.success()),
        "{observed:?}"
    );
    assert!(observed.publications >= 12, "{observed:?}");
    assert!(observed.completed, "{observed:?}");
    assert!(observed.sealed, "{observed:?}");
    assert!(observed.persister_count > 0, "{observed:?}");
    assert!(observed.persister_absent, "{observed:?}");
    harness.remove_successful_fixture();
}

#[test]
fn finishes_recording_without_a_stdout_descriptor() {
    // Goal: an absent inherited stdout leaves actual finite recording able to finish.
    // Method: close stdout before exec; Rust may sanitize it before reporter installation.
    let mut harness = Harness::new("absent_stdout");

    harness.start(Output::Absent);
    let observed = harness.observe();
    harness.cleanup().unwrap();

    assert!(
        observed.status.is_some_and(|status| status.success()),
        "{observed:?}"
    );
    assert!(observed.publications >= 12, "{observed:?}");
    assert!(observed.completed, "{observed:?}");
    assert!(observed.sealed, "{observed:?}");
    assert!(observed.persister_count > 0, "{observed:?}");
    assert!(observed.persister_absent, "{observed:?}");
    harness.remove_successful_fixture();
}
