use crate::ring::{IdentityAvailability, IdentityUnavailable, ProcessCreation};

use super::super::{Acquisition, CreationError, PathError};

pub(super) fn stable_creation(acquired: &Acquisition) -> Option<ProcessCreation> {
    if let (Ok(before), Ok(after)) = (acquired.creation_before, acquired.creation_after) {
        if before == after {
            return Some(before);
        }
    }

    None
}

pub(super) fn failure_reason(acquired: &Acquisition) -> IdentityUnavailable {
    match acquired.creation_before {
        Ok(before) => match acquired.creation_after {
            Ok(after) => {
                if before != after {
                    return IdentityUnavailable::Changed;
                }
            }
            Err(error) => return creation_reason(error),
        },
        Err(error) => return creation_reason(error),
    }

    // A missing fresh right proves no permission or unsupported-flavor errno.
    if acquired.readiness.is_err() {
        return IdentityUnavailable::Unsupported;
    }

    match acquired.path {
        Some(Err(error)) => path_reason(error),
        _ => IdentityUnavailable::Invalid,
    }
}

fn creation_reason(error: CreationError) -> IdentityUnavailable {
    match error {
        CreationError::Refused(_) => IdentityUnavailable::Refused,
        CreationError::Gone(_) => IdentityUnavailable::Gone,
        CreationError::Unsupported(_) | CreationError::Syscall(_) => {
            IdentityUnavailable::Unsupported
        }
        CreationError::Size(_) | CreationError::NotRead => IdentityUnavailable::Invalid,
    }
}

fn path_reason(error: PathError) -> IdentityUnavailable {
    match error {
        PathError::Refused(_) => IdentityUnavailable::Refused,
        PathError::Gone(_) => IdentityUnavailable::Gone,
        PathError::Unsupported(_) | PathError::Syscall(_) => IdentityUnavailable::Unsupported,
        PathError::Length(_)
        | PathError::TooLong
        | PathError::Terminator
        | PathError::EmbeddedNul
        | PathError::NotAbsolute
        | PathError::EmptyBasename => IdentityUnavailable::Invalid,
    }
}

pub(super) fn dictionary_reason(reason: IdentityAvailability) -> IdentityUnavailable {
    match reason {
        IdentityAvailability::Measured => unreachable!("intern cannot refuse with Measured"),
        IdentityAvailability::Refused => IdentityUnavailable::Refused,
        IdentityAvailability::Gone => IdentityUnavailable::Gone,
        IdentityAvailability::Unsupported => IdentityUnavailable::Unsupported,
        IdentityAvailability::Changed => IdentityUnavailable::Changed,
        IdentityAvailability::Invalid => IdentityUnavailable::Invalid,
        IdentityAvailability::Exhausted => IdentityUnavailable::Exhausted,
    }
}

pub(super) fn unexpected_syscalls(acquired: &Acquisition) -> u64 {
    u64::from(matches!(
        acquired.creation_before,
        Err(CreationError::Syscall(_))
    )) + u64::from(matches!(
        acquired.creation_after,
        Err(CreationError::Syscall(_))
    )) + u64::from(matches!(acquired.path, Some(Err(PathError::Syscall(_)))))
}
