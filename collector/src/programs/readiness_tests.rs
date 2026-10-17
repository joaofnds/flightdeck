use std::cell::Cell;

use super::ReadinessError;
use super::readiness::acquire_readiness_result;

thread_local! { static RELEASES: Cell<u32> = const { Cell::new(0) }; }

pub(super) fn release(name: u32) -> i32 {
    assert_eq!(name, 7);
    RELEASES.set(RELEASES.get() + 1);
    0
}

pub(super) fn releases() -> u32 {
    RELEASES.get()
}

pub(super) fn reset_releases() {
    RELEASES.set(0);
}

#[test]
fn rejects_a_successful_null_task_name() {
    // Goal: KERN_SUCCESS alone is insufficient readiness evidence.
    // Method: decode the documented successful-NULL case without releasing a sentinel.
    reset_releases();
    let mut output = None;

    assert_eq!(
        Err(ReadinessError::Null),
        acquire_readiness_result(0, 0, &mut output, release)
    );
    assert!(output.is_none());
    assert_eq!(0, releases());
}

#[test]
fn rejects_a_successful_dead_task_name() {
    // Goal: the dead-name sentinel never becomes an owned readiness right.
    // Method: decode success with MACH_PORT_DEAD and count releases.
    reset_releases();
    let mut output = None;

    assert_eq!(
        Err(ReadinessError::Dead),
        acquire_readiness_result(0, u32::MAX, &mut output, release)
    );
    assert!(output.is_none());
    assert_eq!(0, releases());
}

macro_rules! kernel_failures {
    ($($name:ident, $port:expr, $releases:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: gate failure retains raw status and releases only a valid returned right.
            // Method: decode one listed failed-call name and count its exact releases.
            reset_releases();
            let mut output = None;

            assert_eq!(Err(ReadinessError::Kernel { status: 5, name: $port }),
                acquire_readiness_result(5, $port, &mut output, release));

            assert!(output.is_none());
            assert_eq!($releases, releases());
        }
    )*};
}

kernel_failures! {
    retains_a_failed_null_task_name_without_releasing_it, 0, 0;
    retains_a_failed_dead_task_name_without_releasing_it, u32::MAX, 0;
    releases_an_unexpected_valid_name_from_a_failed_call, 7, 1;
}

#[test]
fn releases_a_valid_readiness_right_exactly_once() {
    // Goal: caller scope owns one right, including replacement of prior output.
    // Method: acquire twice into the same storage and then drop its final right.
    reset_releases();
    let mut output = None;

    acquire_readiness_result(0, 7, &mut output, release).unwrap();
    assert_eq!(0, releases());
    acquire_readiness_result(0, 7, &mut output, release).unwrap();
    assert_eq!(1, releases());
    drop(output);
    assert_eq!(2, releases());
}
