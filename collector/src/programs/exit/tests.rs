use std::cell::Cell;

use super::*;

const CREATION: ProcessCreation = ProcessCreation {
    unique_id: 71,
    pid_version: 9,
};

#[test]
fn retains_creation_that_brackets_final_usage() {
    // Goal: an exited process keeps verified creation without requiring path or task readiness.
    // Method: acquire final numeric data between stable exited-inclusive source reads.
    let numeric = Cell::new(0);
    let mut output = ExitAcquisition::EMPTY;

    acquire_exit_using(
        42,
        &mut output,
        |pid, include_exited, creation| {
            assert_eq!(42, pid);
            assert!(include_exited);
            *creation = Ok(CREATION);
        },
        || {
            numeric.set(99);
            Some(true)
        },
    );

    assert_eq!(99, numeric.get());
    assert_eq!(Ok(CREATION), output.creation());
}

macro_rules! bracket_cases {
    ($($name:ident, $before:expr, $after:expr, $expected:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: identity failure never discards final numeric data or invents creation.
            // Method: change source state at the numeric boundary and retain raw failures.
            let numeric = Cell::new(0);
            let mut output = ExitAcquisition::EMPTY;

            acquire_exit_using(42, &mut output, |pid, include_exited, creation| {
                assert_eq!(42, pid);
                assert!(include_exited);
                *creation = if numeric.get() == 0 { $before } else { $after };
            }, || { numeric.set(99); Some(true) });

            assert_eq!(99, numeric.get());
            assert_eq!($before, output.creation_before);
            assert_eq!($after, output.creation_after);
            assert_eq!($expected, output.creation());
        }
    )*};
}

bracket_cases! {
    rejects_unique_id_reuse_during_final_read, Ok(CREATION),
        Ok(ProcessCreation { unique_id: 72, ..CREATION }), Err(IdentityUnavailable::Changed);
    rejects_exec_during_final_read, Ok(CREATION),
        Ok(ProcessCreation { pid_version: 10, ..CREATION }), Err(IdentityUnavailable::Changed);
    retains_numeric_after_a_refused_first_identity, Err(CreationError::Refused(1)),
        Ok(CREATION), Err(IdentityUnavailable::Refused);
    retains_numeric_after_a_gone_first_identity, Err(CreationError::Gone(3)),
        Ok(CREATION), Err(IdentityUnavailable::Gone);
    rejects_a_gone_second_identity, Ok(CREATION), Err(CreationError::Gone(3)),
        Err(IdentityUnavailable::Gone);
    rejects_a_refused_second_identity, Ok(CREATION), Err(CreationError::Refused(13)),
        Err(IdentityUnavailable::Refused);
    retains_numeric_when_identity_is_unsupported, Err(CreationError::Unsupported(22)),
        Err(CreationError::Unsupported(22)), Err(IdentityUnavailable::Unsupported);
    retains_unexpected_syscall_statuses, Ok(CREATION), Err(CreationError::Syscall(14)),
        Err(IdentityUnavailable::Unsupported);
    retains_invalid_native_width, Err(CreationError::Size(55)), Ok(CREATION),
        Err(IdentityUnavailable::Invalid);
}

#[test]
fn brackets_final_numeric_reads_without_heap_activity() {
    // Goal: source bracketing requires no dynamic storage after startup.
    // Method: count heap calls across repeated successful and gone source outcomes.
    let mut output = ExitAcquisition::EMPTY;
    let mut numeric = 0;

    let allocations = crate::recording::discovery::tests::allocator_calls_in(|| {
        for source in [Ok(CREATION), Err(CreationError::Gone(3))] {
            acquire_exit_using(
                42,
                &mut output,
                |_, _, creation| *creation = source,
                || {
                    numeric += 1;
                    Some(true)
                },
            );
        }
    });

    assert_eq!(0, allocations);
    assert_eq!(2, numeric);
}

#[test]
fn rejects_a_live_replacement_with_stable_creation_brackets() {
    // Goal: a stale exit notice cannot turn a reused live PID into verified final usage.
    // Method: keep creation stable but return the same numeric sample's false exit witness.
    let mut output = ExitAcquisition::EMPTY;

    acquire_exit_using(
        42,
        &mut output,
        |_, _, creation| *creation = Ok(CREATION),
        || Some(false),
    );

    assert_eq!(Err(IdentityUnavailable::Changed), output.creation());
}

#[test]
fn leaves_creation_unavailable_when_final_usage_cannot_witness_exit() {
    // Goal: unreadable final counters cannot establish the current PID is the exited process.
    // Method: keep both creation reads stable while the numeric read has no exit witness.
    let mut output = ExitAcquisition::EMPTY;

    acquire_exit_using(
        42,
        &mut output,
        |_, _, creation| *creation = Ok(CREATION),
        || None,
    );

    assert_eq!(Err(IdentityUnavailable::Unsupported), output.creation());
}
