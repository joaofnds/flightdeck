use std::cell::Cell;

use crate::ring::ProcessCreation;

use super::acquisition::{ReadCalls, acquire_using};
use super::readiness::acquire_readiness_result;
use super::readiness_tests::{release, releases, reset_releases};
use super::{Acquisition, CreationError, PathError, Readiness, ReadinessError};

struct Calls<'log> {
    creations: [Result<ProcessCreation, CreationError>; 2],
    creation_index: usize,
    gate_status: i32,
    path: Result<usize, PathError>,
    log: &'log Cell<u32>,
}

impl ReadCalls for Calls<'_> {
    fn creation(
        &mut self,
        pid: i32,
        include_exited: bool,
        output: &mut Result<ProcessCreation, CreationError>,
    ) {
        assert_eq!(pid, 42);
        assert!(include_exited);
        self.log.set(self.log.get() * 10 + 1);
        *output = self.creations[self.creation_index];
        self.creation_index += 1;
    }

    fn readiness(
        &mut self,
        pid: i32,
        output: &mut Option<Readiness>,
    ) -> Result<(), ReadinessError> {
        assert_eq!(pid, 42);
        self.log.set(self.log.get() * 10 + 2);
        acquire_readiness_result(self.gate_status, 7, output, release)
    }

    fn path(&mut self, pid: i32, scratch: &mut [u8; 4096]) -> Result<usize, PathError> {
        assert_eq!(pid, 42);
        self.log.set(self.log.get() * 10 + 4);
        scratch[..4].copy_from_slice(b"/x\xff\0");
        self.path
    }
}

const CREATION: ProcessCreation = ProcessCreation {
    unique_id: 71,
    pid_version: 9,
};

#[test]
fn rejects_a_changed_unique_id_after_the_path_read() {
    // Goal: a PID reuse bracket mismatch cannot yield an executable candidate.
    // Method: script a changed unique ID around a successful gate, numeric and path read.
    reset_releases();
    let log = Cell::new(0);
    let mut calls = Calls {
        creations: [
            Ok(CREATION),
            Ok(ProcessCreation {
                unique_id: 72,
                ..CREATION
            }),
        ],
        creation_index: 0,
        gate_status: 0,
        path: Ok(3),
        log: &log,
    };
    let mut scratch = [0; 4096];
    let mut output = Acquisition::EMPTY;
    let mut numeric = 0;

    acquire_using(&mut calls, 42, true, &mut scratch, &mut output, || {
        log.set(log.get() * 10 + 3);
        numeric = 99;
    });

    assert!(!output.candidate);
    assert_eq!(99, numeric);
    assert_eq!(12341, log.get());
    assert_eq!(1, releases());
}

macro_rules! attempts {
    ($($name:ident, $before:expr, $after:expr, $gate:expr, $path:expr,
        $candidate:expr, $sequence:expr, $expected_path:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: each identity outcome preserves numeric data and independent creation.
            // Method: run one fixed bracket scenario and check order, output and right release.
            reset_releases();
            let log = Cell::new(0);
            let mut calls = Calls { creations: [$before, $after], creation_index: 0,
                gate_status: $gate, path: $path, log: &log };
            let mut scratch = [0; 4096];
            let mut output = Acquisition::EMPTY;
            let mut numeric = 0;

            acquire_using(&mut calls, 42, true, &mut scratch, &mut output, || {
                log.set(log.get() * 10 + 3);
                numeric = 99;
            });

            assert_eq!($candidate, output.candidate);
            assert_eq!(99, numeric);
            assert_eq!($sequence, log.get());
            assert_eq!($before, output.creation_before);
            assert_eq!($after, output.creation_after);
            assert_eq!($expected_path, output.path);
            assert_eq!(1, releases());
        }
    )*};
}

attempts! {
    accepts_an_unchanged_bracket, Ok(CREATION), Ok(CREATION), 0, Ok(3),
        true, 12341, Some(Ok(3));
    rejects_a_changed_pid_version, Ok(CREATION),
        Ok(ProcessCreation { pid_version: 10, ..CREATION }), 0, Ok(3),
        false, 12341, Some(Ok(3));
    retains_numeric_when_creation_before_is_gone, Err(CreationError::Gone(3)),
        Ok(CREATION), 0, Ok(3), false, 12341, Some(Ok(3));
    retains_creation_before_when_after_is_refused, Ok(CREATION),
        Err(CreationError::Refused(1)), 0, Ok(3), false, 12341, Some(Ok(3));
    retains_numeric_when_path_is_refused, Ok(CREATION), Ok(CREATION), 0,
        Err(PathError::Refused(1)), false, 12341, Some(Err(PathError::Refused(1)));
    retains_numeric_when_the_gate_fails, Ok(CREATION), Ok(CREATION), 5, Ok(3),
        false, 1231, None;
    refuses_a_structurally_invalid_path, Ok(CREATION), Ok(CREATION), 0, Ok(1),
        false, 12341, Some(Err(PathError::EmptyBasename));
}

#[test]
fn acquisition_reuses_only_caller_owned_storage() {
    // Goal: the primitive coordinator allocates no dynamic memory on any repeated attempt.
    // Method: count allocation and free calls for successful and failed scripted attempts.
    let log = Cell::new(0);
    let mut scratch = [0; 4096];
    let mut output = Acquisition::EMPTY;
    let mut numeric = 0;
    let allocations = crate::recording::discovery::tests::allocator_calls_in(|| {
        for gate_status in [0, 5] {
            log.set(0);
            let mut calls = Calls {
                creations: [Ok(CREATION), Ok(CREATION)],
                creation_index: 0,
                gate_status,
                path: Ok(3),
                log: &log,
            };
            acquire_using(&mut calls, 42, true, &mut scratch, &mut output, || {
                numeric += 1;
            });
        }
    });

    assert_eq!(0, allocations);
    assert_eq!(2, numeric);
}
