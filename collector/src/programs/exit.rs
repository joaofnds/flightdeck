use crate::ring::{IdentityUnavailable, ProcessCreation};

use super::{CreationError, creation_reason};

#[derive(Debug)]
pub struct ExitAcquisition {
    pub creation_before: Result<ProcessCreation, CreationError>,
    pub creation_after: Result<ProcessCreation, CreationError>,
    /// None means unavailable numeric data; true requires that sample's nonzero exit time.
    pub exited: Option<bool>,
}

impl ExitAcquisition {
    pub const EMPTY: Self = Self {
        creation_before: Err(CreationError::NotRead),
        creation_after: Err(CreationError::NotRead),
        exited: None,
    };

    pub fn creation(&self) -> Result<ProcessCreation, IdentityUnavailable> {
        let before = self.creation_before.map_err(creation_reason)?;
        let after = self.creation_after.map_err(creation_reason)?;

        if before != after {
            return Err(IdentityUnavailable::Changed);
        }

        match self.exited {
            Some(true) => Ok(before),
            Some(false) => Err(IdentityUnavailable::Changed),
            None => Err(IdentityUnavailable::Unsupported),
        }
    }
}

/// Numeric data is retained by the caller independently of creation availability.
pub fn acquire_exit(
    pid: i32,
    output: &mut ExitAcquisition,
    numeric: impl FnOnce() -> Option<bool>,
) {
    acquire_exit_using(pid, output, super::read_creation, numeric);

    acquire_exit_report(pid, output.creation_before);
    acquire_exit_report(pid, output.creation_after);
}

fn acquire_exit_using(
    pid: i32,
    output: &mut ExitAcquisition,
    mut creation: impl FnMut(i32, bool, &mut Result<ProcessCreation, CreationError>),
    numeric: impl FnOnce() -> Option<bool>,
) {
    *output = ExitAcquisition::EMPTY;
    creation(pid, true, &mut output.creation_before);
    output.exited = numeric();
    creation(pid, true, &mut output.creation_after);
}

fn acquire_exit_report(pid: i32, creation: Result<ProcessCreation, CreationError>) {
    if let Err(error @ (CreationError::Syscall(_) | CreationError::Size(_))) = creation {
        eprintln!("collector: exit creation read pid={pid} failed: {error:?}");
    }
}

#[cfg(test)]
mod tests;
