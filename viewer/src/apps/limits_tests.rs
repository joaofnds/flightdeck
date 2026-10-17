use super::tests::{interval, observation, program, usage};
use super::*;

#[test]
fn refuses_oversized_result_input_and_clears_prior_output() {
    // Goal: an oversized operation cannot leave a partial or stale complete result.
    // Method: compute a good operation, then exceed the explicit row bound by one.
    let interval = interval();
    let program = program(b"/A.app");
    let usage = usage(1, 2, 3);
    let selected = observation(&interval.selected, &program, &usage);
    let mut result = tests::aggregate();
    result
        .compute(
            &interval,
            &[ProcessInterval {
                previous: None,
                selected: &selected,
                start: None,
            }],
        )
        .unwrap();
    let spans: Vec<_> = (0..=PROCESS_INTERVALS_MAX)
        .map(|_| ProcessInterval {
            previous: None,
            selected: &selected,
            start: None,
        })
        .collect();

    let error = result.compute(&interval, &spans);

    assert_eq!(error, Err(Error::ProcessBound));
    assert!(result.apps.is_empty());
    assert!(result.processes.is_empty());
    assert!(!result.membership_complete);
}

#[test]
fn refuses_a_program_byte_budget_exceeded_by_valid_repeated_names() {
    // Goal: row-count limits cannot bypass the independent captured-byte operation bound.
    // Method: submit maximum-length valid metadata on both endpoints of 8195 distinct members.
    let interval = interval();
    let name = [b'x'; 4095];
    let program = ProgramBytes {
        executable_name: &name,
        app_path: None,
    };
    let usage = usage(1, 2, 3);
    let before: Vec<_> = (0..8195)
        .map(|index| {
            let mut value = observation(&interval.previous, &program, &usage);
            value.pid = index;
            value
        })
        .collect();
    let selected: Vec<_> = (0..8195)
        .map(|index| {
            let mut value = observation(&interval.selected, &program, &usage);
            value.pid = index;
            value
        })
        .collect();
    let spans: Vec<_> = before
        .iter()
        .zip(&selected)
        .map(|(previous, selected)| ProcessInterval {
            previous: Some(previous),
            selected,
            start: None,
        })
        .collect();
    let mut result = tests::aggregate();

    let error = result.compute(&interval, &spans);

    assert_eq!(error, Err(Error::ProgramBound));
    assert!(result.apps.is_empty());
    assert!(result.processes.is_empty());
}

#[test]
fn refuses_oversized_original_program_bytes() {
    // Goal: no caller can bypass the shared captured-program maximum.
    // Method: submit a basename one byte beyond the retained metadata contract.
    let interval = interval();
    let name = [b'x'; 4096];
    let program = ProgramBytes {
        executable_name: &name,
        app_path: None,
    };
    let usage = usage(1, 2, 3);
    let selected = observation(&interval.selected, &program, &usage);
    let mut result = tests::aggregate();

    let error = result.compute(
        &interval,
        &[ProcessInterval {
            previous: None,
            selected: &selected,
            start: None,
        }],
    );

    assert_eq!(error, Err(Error::InvalidProgram));
}

#[test]
fn refuses_duplicate_creation_members() {
    // Goal: duplicated rows cannot double an App's measured total.
    // Method: submit the same selected creation twice in a bounded operation.
    let interval = interval();
    let program = program(b"/A.app");
    let usage = usage(1, 2, 3);
    let selected = observation(&interval.selected, &program, &usage);
    let spans = [
        ProcessInterval {
            previous: None,
            selected: &selected,
            start: None,
        },
        ProcessInterval {
            previous: None,
            selected: &selected,
            start: None,
        },
    ];
    let mut result = tests::aggregate();

    let error = result.compute(&interval, &spans);

    assert_eq!(error, Err(Error::DuplicateProcess));
}

#[test]
fn refuses_a_predecessor_with_mismatched_source_identity() {
    // Goal: a caller-provided join must carry the exact preceding source endpoint.
    // Method: supply otherwise equal counters with a predecessor from another run.
    let interval = interval();
    let program = program(b"/A.app");
    let usage = usage(1, 2, 3);
    let mut previous = observation(&interval.previous, &program, &usage);
    previous.stamp.run = [9, 9];
    let selected = observation(&interval.selected, &program, &usage);
    let mut result = tests::aggregate();

    let error = result.compute(
        &interval,
        &[ProcessInterval {
            previous: Some(&previous),
            selected: &selected,
            start: None,
        }],
    );

    assert_eq!(error, Err(Error::InvalidEndpoint));
}

#[test]
fn reports_cpu_sum_overflow_without_erasing_valid_energy() {
    // Goal: an unrepresentable CPU delta cannot panic or corrupt an independent energy delta.
    // Method: increase CPU components by u64::MAX plus one while energy increases by ten.
    let interval = interval();
    let program = program(b"/A.app");
    let before_usage = usage(0, 0, 1);
    let mut selected_usage = usage(u64::MAX, 10, 2);
    selected_usage.system_ns = 1;
    let previous = observation(&interval.previous, &program, &before_usage);
    let selected = observation(&interval.selected, &program, &selected_usage);
    let mut result = tests::aggregate();

    result
        .compute(
            &interval,
            &[ProcessInterval {
                previous: Some(&previous),
                selected: &selected,
                start: None,
            }],
        )
        .unwrap();

    assert_eq!(
        result.processes[0].cpu_ns,
        Amount::Unavailable(Unavailable::Overflow)
    );
    assert_eq!(result.processes[0].energy_nj, Amount::Known(10));
}

#[test]
fn retains_program_refusal_and_marks_every_app_membership_incomplete() {
    // Goal: unattributed usage remains visible and cannot certify any App membership set.
    // Method: put a path-refused process after a known App in input order.
    let interval = interval();
    let program = program(b"/A.app");
    let usage = usage(1, 2, 3);
    let known = observation(&interval.selected, &program, &usage);
    let mut refused = observation(&interval.selected, &program, &usage);
    refused.pid = 8;
    refused.program = Err(IdentityUnavailable::Refused);
    let mut result = tests::aggregate();

    result
        .compute(
            &interval,
            &[
                ProcessInterval {
                    previous: None,
                    selected: &known,
                    start: None,
                },
                ProcessInterval {
                    previous: None,
                    selected: &refused,
                    start: None,
                },
            ],
        )
        .unwrap();

    assert_eq!(result.ungrouped_members, 1);
    assert_eq!(
        result.processes[1].program_unavailable,
        Some(IdentityUnavailable::Refused)
    );
    assert!(!result.apps[&AppKey::Bundle(b"/A.app")].membership_complete);
}

#[test]
fn reports_an_overflowing_app_gauge_as_a_partial_known_subtotal() {
    // Goal: adding live gauges beyond u64 cannot panic or masquerade as a complete measurement.
    // Method: aggregate resident readings of u64::MAX and one in the same App.
    let interval = interval();
    let program = program(b"/A.app");
    let mut large_usage = usage(1, 2, 3);
    large_usage.resident_bytes = u64::MAX;
    let small_usage = usage(1, 2, 1);
    let large = observation(&interval.selected, &program, &large_usage);
    let mut small = observation(&interval.selected, &program, &small_usage);
    small.pid = 8;
    let mut result = tests::aggregate();

    result
        .compute(
            &interval,
            &[
                ProcessInterval {
                    previous: None,
                    selected: &large,
                    start: None,
                },
                ProcessInterval {
                    previous: None,
                    selected: &small,
                    start: None,
                },
            ],
        )
        .unwrap();

    let resident = &result.apps[&AppKey::Bundle(b"/A.app")].resident_bytes;
    assert_eq!(resident.known, u64::MAX);
    assert!(resident.overflowed);
    assert_eq!(resident.measured_members, 1);
    assert_eq!(resident.unavailable_members, 1);
}
