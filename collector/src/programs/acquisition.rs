use crate::ring::ProcessCreation;

use super::{CreationError, PathError, Readiness, ReadinessError, split_program};

/// Caller-owned diagnostics. A candidate is conditional evidence, not published identity.
pub struct Acquisition {
    pub creation_before: Result<ProcessCreation, CreationError>,
    pub creation_after: Result<ProcessCreation, CreationError>,
    pub readiness: Result<(), ReadinessError>,
    pub path: Option<Result<usize, PathError>>,
    pub candidate: bool,
}

impl Acquisition {
    pub const EMPTY: Self = Self {
        creation_before: Err(CreationError::NotRead),
        creation_after: Err(CreationError::NotRead),
        readiness: Err(ReadinessError::NotRead),
        path: None,
        candidate: false,
    };
}

/// Makes one bounded attempt. The numeric callback always runs, even if identity fails.
/// The caller retains numeric output independently and owns the 4096-byte path scratch.
pub fn acquire(
    pid: i32,
    include_exited: bool,
    scratch: &mut [u8; 4096],
    output: &mut Acquisition,
    numeric: impl FnOnce(),
) {
    acquire_using(&mut Native, pid, include_exited, scratch, output, numeric);
}

pub(super) trait ReadCalls {
    fn creation(
        &mut self,
        pid: i32,
        include_exited: bool,
        output: &mut Result<ProcessCreation, CreationError>,
    );
    fn readiness(&mut self, pid: i32, output: &mut Option<Readiness>)
    -> Result<(), ReadinessError>;
    fn path(&mut self, pid: i32, scratch: &mut [u8; 4096]) -> Result<usize, PathError>;
}

pub(super) fn acquire_using(
    calls: &mut impl ReadCalls,
    pid: i32,
    include_exited: bool,
    scratch: &mut [u8; 4096],
    output: &mut Acquisition,
    numeric: impl FnOnce(),
) {
    *output = Acquisition::EMPTY;
    calls.creation(pid, include_exited, &mut output.creation_before);

    let mut readiness = None;
    output.readiness = calls.readiness(pid, &mut readiness);

    numeric();
    if output.readiness.is_ok() {
        output.path = Some(acquire_path(calls, pid, scratch));
    }
    calls.creation(pid, include_exited, &mut output.creation_after);

    drop(readiness);
    if matches!(output.path, Some(Ok(_))) {
        if let (Ok(before), Ok(after)) = (&output.creation_before, &output.creation_after) {
            output.candidate = before == after;
        }
    }
}

fn acquire_path(
    calls: &mut impl ReadCalls,
    pid: i32,
    scratch: &mut [u8; 4096],
) -> Result<usize, PathError> {
    let length = calls.path(pid, scratch)?;
    let bytes = scratch.get(..length).ok_or(PathError::TooLong)?;
    let mut program = None;
    split_program(bytes, &mut program)?;
    Ok(length)
}

struct Native;

impl ReadCalls for Native {
    fn creation(
        &mut self,
        pid: i32,
        include_exited: bool,
        output: &mut Result<ProcessCreation, CreationError>,
    ) {
        super::read_creation(pid, include_exited, output);
    }

    fn readiness(
        &mut self,
        pid: i32,
        output: &mut Option<Readiness>,
    ) -> Result<(), ReadinessError> {
        super::acquire_readiness(pid, output)
    }

    fn path(&mut self, pid: i32, scratch: &mut [u8; 4096]) -> Result<usize, PathError> {
        super::read_path(pid, scratch)
    }
}
